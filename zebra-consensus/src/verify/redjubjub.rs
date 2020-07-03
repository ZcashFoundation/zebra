use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use rand::thread_rng;
use redjubjub::*;
use tokio::sync::broadcast::{channel, RecvError, Sender};
use tower::Service;
use tower_batch::BatchControl;

/// RedJubjub signature verifier service
pub struct RedJubjubVerifier {
    batch: batch::Verifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

#[allow(clippy::new_without_default)]
impl RedJubjubVerifier {
    /// Create a new RedJubjubVerifier instance
    pub fn new() -> Self {
        let batch = batch::Verifier::default();
        // XXX(hdevalence) what's a reasonable choice here?
        let (tx, _) = channel(10);
        Self { tx, batch }
    }
}

/// Type alias to clarify that this batch::Item is a RedJubjubItem
pub type RedJubjubItem = batch::Item;

impl Service<BatchControl<RedJubjubItem>> for RedJubjubVerifier {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<RedJubjubItem>) -> Self::Future {
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

impl Drop for RedJubjubVerifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(batch.verify(thread_rng()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use color_eyre::eyre::Result;
    use futures::stream::{FuturesUnordered, StreamExt};
    use tower::ServiceExt;
    use tower_batch::Batch;

    async fn sign_and_verify<V>(mut verifier: V, n: usize) -> Result<(), V::Error>
    where
        V: Service<RedJubjubItem, Response = ()>,
    {
        let rng = thread_rng();
        let mut results = FuturesUnordered::new();
        for i in 0..n {
            let span = tracing::trace_span!("sig", i);
            let msg = b"BatchVerifyTest";

            match i % 2 {
                0 => {
                    let sk = SigningKey::<SpendAuth>::new(rng);
                    let vk = VerificationKey::from(&sk);
                    let sig = sk.sign(rng, &msg[..]);
                    verifier.ready_and().await?;
                    results.push(span.in_scope(|| verifier.call((vk.into(), sig, msg).into())))
                }
                1 => {
                    let sk = SigningKey::<Binding>::new(rng);
                    let vk = VerificationKey::from(&sk);
                    let sig = sk.sign(rng, &msg[..]);
                    verifier.ready_and().await?;
                    results.push(span.in_scope(|| verifier.call((vk.into(), sig, msg).into())))
                }
                _ => panic!(),
            }
        }

        while let Some(result) = results.next().await {
            result?;
        }

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn batch_flushes_on_max_items() -> Result<()> {
        use tokio::time::timeout;

        // Use a very long max_latency and a short timeout to check that
        // flushing is happening based on hitting max_items.
        let verifier = Batch::new(RedJubjubVerifier::new(), 10, Duration::from_secs(1000));
        timeout(Duration::from_secs(1), sign_and_verify(verifier, 100)).await?
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn batch_flushes_on_max_latency() -> Result<()> {
        use tokio::time::timeout;

        // Use a very high max_items and a short timeout to check that
        // flushing is happening based on hitting max_latency.
        let verifier = Batch::new(RedJubjubVerifier::new(), 100, Duration::from_millis(500));
        timeout(Duration::from_secs(1), sign_and_verify(verifier, 10)).await?
    }
}
