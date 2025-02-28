//! Async Ed25519 batch verifier service

use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
use rand::thread_rng;

use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl, ItemSize};
use tower_fallback::Fallback;
use zebra_chain::primitives::ed25519::*;

use crate::BoxError;

use super::{spawn_fifo, spawn_fifo_and_convert};

#[cfg(test)]
mod tests;

/// The type of the batch verifier.
type BatchVerifier = batch::Verifier;

/// The type of verification results.
type VerifyResult = Result<(), Error>;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/// The type of the batch item.
/// This is a newtype around an `Ed25519Item`.
#[derive(Clone, Debug)]
pub struct Item(batch::Item);

impl ItemSize for Item {}

impl<'msg, M: AsRef<[u8]> + ?Sized> From<(VerificationKeyBytes, Signature, &'msg M)> for Item {
    fn from(tup: (VerificationKeyBytes, Signature, &'msg M)) -> Self {
        Self(batch::Item::from(tup))
    }
}

impl From<Item> for batch::Item {
    fn from(Item(item): Item) -> Self {
        item
    }
}

impl Item {
    fn verify_single(self) -> VerifyResult {
        self.0.verify_single()
    }
}

/// Global batch verification context for Ed25519 signatures.
///
/// This service transparently batches contemporaneous signature verifications,
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
            Verifier::default(),
            super::MAX_BATCH_SIZE,
            None,
            super::MAX_BATCH_LATENCY,
        ),
        // We want to fallback to individual verification if batch verification fails,
        // so we need a Service to use.
        //
        // Because we have to specify the type of a static, we need to be able to
        // write the type of the closure and its return value. But both closures and
        // async blocks have unnameable types. So instead we cast the closure to a function
        // (which is possible because it doesn't capture any state), and use a BoxFuture
        // to erase the result type.
        // (We can't use BoxCloneService to erase the service type, because it is !Sync.)
        tower::service_fn(
            (|item: Item| Verifier::verify_single_spawning(item).boxed()) as fn(_) -> _,
        ),
    )
});

/// Ed25519 signature verifier service
pub struct Verifier {
    /// A batch verifier for ed25519 signatures.
    batch: BatchVerifier,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Default for Verifier {
    fn default() -> Self {
        let batch = BatchVerifier::default();
        let (tx, _) = watch::channel(None);
        Self { batch, tx }
    }
}

impl Verifier {
    /// Returns the batch verifier and channel sender from `self`,
    /// replacing them with a new empty batch.
    fn take(&mut self) -> (BatchVerifier, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut self.batch);

        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, tx)
    }

    /// Synchronously process the batch, and send the result using the channel sender.
    /// This function blocks until the batch is completed.
    fn verify(batch: BatchVerifier, tx: Sender) {
        let result = batch.verify(thread_rng());
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
    async fn flush_spawning(batch: BatchVerifier, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let _ = tx.send(spawn_fifo(move || batch.verify(thread_rng())).await.ok());
    }

    /// Verify a single item using a thread pool, and return the result.
    async fn verify_single_spawning(item: Item) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        spawn_fifo_and_convert(move || item.verify_single()).await
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
                tracing::trace!("got ed25519 item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();

                Box::pin(async move {
                    match rx.changed().await {
                        Ok(()) => {
                            // We use a new channel for each batch,
                            // so we always get the correct batch result here.
                            let result = rx.borrow()
                                .ok_or("threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?")?;

                            if result.is_ok() {
                                tracing::trace!(?result, "validated ed25519 signature");
                                metrics::counter!("signatures.ed25519.validated").increment(1);
                            } else {
                                tracing::trace!(?result, "invalid ed25519 signature");
                                metrics::counter!("signatures.ed25519.invalid").increment(1);
                            }
                            result.map_err(BoxError::from)
                        }
                        Err(_recv_error) => panic!("ed25519 verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got ed25519 flush command");

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
        self.flush_blocking();
    }
}
