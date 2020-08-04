//! Async RedJubjub batch verifier service

#[cfg(test)]
mod tests;

use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use rand::thread_rng;
use redjubjub::{batch, *};
use tokio::sync::broadcast::{channel, RecvError, Sender};
use tower::Service;
use tower_batch::BatchControl;

/// RedJubjub signature verifier service
pub struct Verifier {
    batch: batch::Verifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

#[allow(clippy::new_without_default)]
impl Verifier {
    /// Create a new RedJubjubVerifier instance
    pub fn new() -> Self {
        let batch = batch::Verifier::default();
        // XXX(hdevalence) what's a reasonable choice here?
        let (tx, _) = channel(10);
        Self { tx, batch }
    }
}

/// Type alias to clarify that this batch::Item is a RedJubjubItem
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
                tracing::trace!("got item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();
                Box::pin(async move {
                    match rx.recv().await {
                        Ok(result) => result,
                        Err(RecvError::Lagged(_)) => {
                            tracing::warn!(
                                "missed channel updates for the correct signature batch!"
                            );
                            Err(Error::InvalidSignature)
                        }
                        Err(RecvError::Closed) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got flush command");
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
