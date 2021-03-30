//! Async Ed25519 batch verifier service

#[cfg(test)]
mod tests;

use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{ready, Ready};
use once_cell::sync::Lazy;
use rand::thread_rng;

use tokio::sync::broadcast::{channel, error::RecvError, Sender};
use tower::{util::ServiceFn, Service};
use tower_batch::{Batch, BatchControl};
use tower_fallback::Fallback;
use zebra_chain::primitives::ed25519::{batch, *};

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
        tower::service_fn((|item: Item| ready(item.verify_single())) as fn(_) -> _),
    )
});

/// Ed25519 signature verifier service
pub struct Verifier {
    batch: batch::Verifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

impl Default for Verifier {
    fn default() -> Self {
        let batch = batch::Verifier::default();
        let (tx, _) = channel(super::BROADCAST_BUFFER_SIZE);
        Self { batch, tx }
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
                    match rx.recv().await {
                        Ok(result) => {
                            if result.is_ok() {
                                tracing::trace!(?result, "validated ed25519 signature");
                                metrics::counter!("signatures.ed25519.validated", 1);
                            } else {
                                tracing::trace!(?result, "invalid ed25519 signature");
                                metrics::counter!("signatures.ed25519.invalid", 1);
                            }
                            result
                        }
                        Err(RecvError::Lagged(_)) => {
                            tracing::error!(
                                "ed25519 batch verification receiver lagged and lost verification results"
                            );
                            Err(Error::InvalidSignature)
                        }
                        Err(RecvError::Closed) => {
                            panic!("ed25519 verifier was dropped without flushing")
                        }
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got ed25519 flush command");
                let batch = mem::take(&mut self.batch);
                let _ = self.tx.send(batch.verify(thread_rng()));
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(batch.verify(thread_rng()));
    }
}
