//! Async Ed25519 batch verifier service

use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{ready, Ready};
use once_cell::sync::Lazy;
use rand::thread_rng;

use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch::{Batch, BatchControl};
use tower_fallback::Fallback;
use zebra_chain::primitives::ed25519::{batch, *};

#[cfg(test)]
mod tests;

/// Global batch verification context for Ed25519 signatures.
///
/// This service transparently batches contemporaneous signature verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static VERIFIER: Lazy<
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> Ready<Result<(), Error>>>>,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::default(),
            super::MAX_BATCH_SIZE,
            super::MAX_BATCH_LATENCY,
        ),
        // We want to fallback to individual verification if batch verification
        // fails, so we need a Service to use. The obvious way to do this would
        // be to write a closure that returns an async block. But because we
        // have to specify the type of a static, we need to be able to write the
        // type of the closure and its return value, and both closures and async
        // blocks have eldritch types whose names cannot be written. So instead,
        // we use a Ready to avoid an async block and cast the closure to a
        // function (which is possible because it doesn't capture any state).
        tower::service_fn(
            (|item: Item| {
                ready(
                    // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
                    //
                    // TODO: use spawn_blocking to avoid blocking code running concurrently in this task
                    tokio::task::block_in_place(|| item.verify_single()),
                )
            }) as fn(_) -> _,
        ),
    )
});

/// Ed25519 signature verifier service
pub struct Verifier {
    /// A batch verifier for ed25519 signatures.
    batch: batch::Verifier,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: watch::Sender<Option<Result<(), Error>>>,
}

impl Default for Verifier {
    fn default() -> Self {
        let batch = batch::Verifier::default();
        let (tx, _) = watch::channel(None);
        Self { batch, tx }
    }
}

impl Verifier {
    /// Flush the batch and return the result via the channel
    fn flush(&mut self) {
        let batch = mem::take(&mut self.batch);

        // # Correctness
        //
        // Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // TODO: use spawn_blocking to avoid blocking code running concurrently in this task
        let result = tokio::task::block_in_place(|| batch.verify(thread_rng()));
        let _ = self.tx.send(Some(result));

        // Use a new channel for each batch.
        let (tx, _) = watch::channel(None);
        let _ = mem::replace(&mut self.tx, tx);
    }
}

/// Type alias to clarify that this `batch::Item` is a `Ed25519Item`
pub type Item = batch::Item;

impl Service<BatchControl<Item>> for Verifier {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>;

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
                            let result = rx.borrow().expect("completed batch must send a value");

                            if result.is_ok() {
                                tracing::trace!(?result, "validated ed25519 signature");
                                metrics::counter!("signatures.ed25519.validated", 1);
                            } else {
                                tracing::trace!(?result, "invalid ed25519 signature");
                                metrics::counter!("signatures.ed25519.invalid", 1);
                            }
                            result
                        }
                        Err(_recv_error) => panic!("ed25519 verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got ed25519 flush command");

                self.flush();

                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        self.flush();
    }
}
