//! Async Tachyon batch verifier service

use std::{
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
use rand::thread_rng;
use zcash_tachyon::{ActionDigest, BatchValidator, Multiset, Stamp};

use crate::BoxError;
use thiserror::Error;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use tower_fallback::Fallback;

use super::spawn_fifo;

/// Adjusted batch size for tachyon batches.
const TACHYON_MAX_BATCH_SIZE: usize = super::MAX_BATCH_SIZE;

/// The type of verification results.
type VerifyResult = bool;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/// A Tachyon verification item, used as the request type of the service.
#[derive(Clone, Debug)]
pub struct Item {
    stamp: Stamp,
    actions_multiset: Multiset<ActionDigest>,
}

impl RequestWeight for Item {
    fn request_weight(&self) -> usize {
        1
    }
}

impl Item {
    /// Creates a new [`Item`] from a stamp and actions multiset.
    pub fn new(stamp: Stamp, actions_multiset: Multiset<ActionDigest>) -> Self {
        Self {
            stamp,
            actions_multiset,
        }
    }

    /// Perform non-batched verification of this [`Item`].
    ///
    /// This is useful (in combination with `Item::clone`) for implementing
    /// fallback logic when batch verification fails.
    pub fn verify_single(self) -> bool {
        let mut batch = BatchValidator::new();
        batch.queue(self);
        batch.validate(thread_rng())
    }
}

trait QueueBatchVerify {
    fn queue(&mut self, item: Item);
}

impl QueueBatchVerify for BatchValidator {
    fn queue(
        &mut self,
        Item {
            stamp,
            actions_multiset,
        }: Item,
    ) {
        self.add_bundle(stamp, actions_multiset);
    }
}

/// An error that may occur when verifying Tachyon stamp proofs.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum TachyonError {
    #[error("tachyon stamp verification failed")]
    VerificationFailed,
}

/// Global batch verification context for Tachyon stamp proofs.
///
/// This service transparently batches contemporaneous proof verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static VERIFIER: Lazy<
    Fallback<
        Batch<Verifier, Item>,
        ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), BoxError>>>,
    >,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::new(),
            TACHYON_MAX_BATCH_SIZE,
            None,
            super::MAX_BATCH_LATENCY,
        ),
        // We want to fallback to individual verification if batch verification fails,
        // so we need a Service to use.
        tower::service_fn(
            (|item: Item| Verifier::verify_single_spawning(item).boxed()) as fn(_) -> _,
        ),
    )
});

/// Tachyon stamp proof verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Tachyon verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    /// The synchronous Tachyon batch validator.
    batch: BatchValidator,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Verifier {
    fn new() -> Self {
        let batch = BatchValidator::new();
        let (tx, _) = watch::channel(None);
        Self { batch, tx }
    }

    /// Returns the batch verifier and channel sender from `self`,
    /// replacing them with a new empty batch.
    fn take(&mut self) -> (BatchValidator, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut self.batch);

        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, tx)
    }

    /// Synchronously process the batch, and send the result using the channel sender.
    /// This function blocks until the batch is completed.
    fn verify(batch: BatchValidator, tx: Sender) {
        let result = batch.validate(thread_rng());
        let _ = tx.send(Some(result));
    }

    /// Flush the batch using a thread pool, and return the result via the channel.
    /// This returns immediately, usually before the batch is completed.
    fn flush_blocking(&mut self) {
        let (batch, tx) = self.take();

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // We don't care about execution order here, because this method is only called on drop.
        tokio::task::block_in_place(|| rayon::spawn_fifo(|| Self::verify(batch, tx)));
    }

    /// Flush the batch using a thread pool, and return the result via the channel.
    /// This function returns a future that becomes ready when the batch is completed.
    async fn flush_spawning(batch: BatchValidator, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let _ = tx.send(
            spawn_fifo(move || batch.validate(thread_rng()))
                .await
                .ok(),
        );
    }

    /// Verify a single item using a thread pool, and return the result.
    async fn verify_single_spawning(item: Item) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        if spawn_fifo(move || item.verify_single()).await? {
            Ok(())
        } else {
            Err("could not validate tachyon stamp proof".into())
        }
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "Verifier";
        f.debug_struct(name)
            .field("batch", &"..")
            .field("tx", &self.tx)
            .finish()
    }
}

impl Service<BatchControl<Item>> for Verifier {
    type Response = ();
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Item>) -> Self::Future {
        match req {
            BatchControl::Item(item) => {
                tracing::trace!("got item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();
                Box::pin(async move {
                    match rx.changed().await {
                        Ok(()) => {
                            // We use a new channel for each batch,
                            // so we always get the correct batch result here.
                            let is_valid = *rx
                                .borrow()
                                .as_ref()
                                .ok_or("threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?")?;

                            if is_valid {
                                tracing::trace!(?is_valid, "verified tachyon stamp proof");
                                metrics::counter!("proofs.tachyon.verified").increment(1);
                                Ok(())
                            } else {
                                tracing::trace!(?is_valid, "invalid tachyon stamp proof");
                                metrics::counter!("proofs.tachyon.invalid").increment(1);
                                Err("could not validate tachyon stamp proofs".into())
                            }
                        }
                        Err(_recv_error) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got tachyon flush command");

                let (batch, tx) = self.take();

                Box::pin(Self::flush_spawning(batch, tx).map(Ok))
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        // This returns immediately, usually before the batch is completed.
        self.flush_blocking()
    }
}
