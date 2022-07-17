//! Async RedJubjub batch verifier service

use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
use rand::thread_rng;

use rayon::prelude::*;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch::{Batch, BatchControl};
use tower_fallback::Fallback;

use zebra_chain::primitives::redjubjub::{batch, *};

#[cfg(test)]
mod tests;

/// The type of the batch verifier.
type BatchVerifier = batch::Verifier;

/// The type of verification results.
type VerifyResult = Result<(), Error>;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/// The type of the batch item.
/// This is a `RedJubjubItem`.
pub type Item = batch::Item;

/// Global batch verification context for RedJubjub signatures.
///
/// This service transparently batches contemporaneous signature verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static VERIFIER: Lazy<
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> BoxFuture<'static, VerifyResult>>>,
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

/// RedJubjub signature verifier service
pub struct Verifier {
    /// A batch verifier for RedJubjub signatures.
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
    fn flush_spawning(batch: BatchVerifier, tx: Sender) -> impl Future<Output = ()> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        tokio::task::spawn_blocking(|| {
            // TODO:
            // - spawn batches so rayon executes them in FIFO order
            //   possible implementation: return a closure in a Future,
            //   then run it using scope_fifo() in the worker task,
            //   limiting the number of concurrent batches to the number of rayon threads
            rayon::scope_fifo(|s| s.spawn_fifo(|_s| Self::verify(batch, tx)))
        })
        .map(|join_result| join_result.expect("panic in redjubjub batch verifier"))
    }

    /// Verify a single item using a thread pool, and return the result.
    /// This function returns a future that becomes ready when the item is completed.
    fn verify_single_spawning(item: Item) -> impl Future<Output = VerifyResult> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        tokio::task::spawn_blocking(|| {
            // Rayon doesn't have a spawn function that returns a value,
            // so we use a parallel iterator instead.
            //
            // TODO:
            // - when a batch fails, spawn all its individual items into rayon using Vec::par_iter()
            // - spawn fallback individual verifications so rayon executes them in FIFO order,
            //   if possible
            rayon::iter::once(item)
                .map(|item| item.verify_single())
                .collect()
        })
        .map(|join_result| join_result.expect("panic in redjubjub fallback verifier"))
    }
}

impl Service<BatchControl<Item>> for Verifier {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = VerifyResult> + Send + 'static>>;

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
                            let result = rx.borrow().expect("completed batch must send a value");

                            if result.is_ok() {
                                tracing::trace!(?result, "validated redjubjub signature");
                                metrics::counter!("signatures.redjubjub.validated", 1);
                            } else {
                                tracing::trace!(?result, "invalid redjubjub signature");
                                metrics::counter!("signatures.redjubjub.invalid", 1);
                            }

                            result
                        }
                        Err(_recv_error) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got redjubjub flush command");

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
