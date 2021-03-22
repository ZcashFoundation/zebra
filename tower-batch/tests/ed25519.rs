use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use color_eyre::{eyre::eyre, Report};
use ed25519_zebra::*;
use futures::stream::{FuturesUnordered, StreamExt};
use rand::thread_rng;
use tokio::sync::broadcast::{channel, error::RecvError, Sender};
use tower::{Service, ServiceExt};
use tower_batch::{Batch, BatchControl};
use tower_fallback::Fallback;

// ============ service impl ============

pub struct Ed25519Verifier {
    batch: batch::Verifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

#[allow(clippy::new_without_default)]
impl Ed25519Verifier {
    pub fn new() -> Self {
        let batch = batch::Verifier::default();
        // XXX(hdevalence) what's a reasonable choice here?
        let (tx, _) = channel(10);
        Self { batch, tx }
    }
}

pub type Ed25519Item = batch::Item;

impl<'msg> Service<BatchControl<Ed25519Item>> for Ed25519Verifier {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Ed25519Item>) -> Self::Future {
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

impl Drop for Ed25519Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(batch.verify(thread_rng()));
    }
}

// =============== testing code ========

async fn sign_and_verify<V>(
    mut verifier: V,
    n: usize,
    bad_index: Option<usize>,
) -> Result<(), V::Error>
where
    V: Service<Ed25519Item, Response = ()>,
{
    let results = FuturesUnordered::new();
    for i in 0..n {
        let span = tracing::trace_span!("sig", i);
        let sk = SigningKey::new(thread_rng());
        let vk_bytes = VerificationKeyBytes::from(&sk);
        let msg = b"BatchVerifyTest";
        let sig = if Some(i) == bad_index {
            sk.sign(b"badmsg")
        } else {
            sk.sign(&msg[..])
        };

        verifier.ready_and().await?;
        results.push(span.in_scope(|| verifier.call((vk_bytes, sig, msg).into())))
    }

    let mut numbered_results = results.enumerate();
    while let Some((i, result)) = numbered_results.next().await {
        if Some(i) == bad_index {
            assert!(result.is_err());
        } else {
            result?;
        }
    }

    Ok(())
}

#[tokio::test]
async fn batch_flushes_on_max_items() -> Result<(), Report> {
    use tokio::time::timeout;
    zebra_test::init();

    // Use a very long max_latency and a short timeout to check that
    // flushing is happening based on hitting max_items.
    let verifier = Batch::new(Ed25519Verifier::new(), 10, Duration::from_secs(1000));
    timeout(Duration::from_secs(1), sign_and_verify(verifier, 100, None))
        .await
        .map_err(|e| eyre!(e))?
        .map_err(|e| eyre!(e))?;

    Ok(())
}

#[tokio::test]
async fn batch_flushes_on_max_latency() -> Result<(), Report> {
    use tokio::time::timeout;
    zebra_test::init();

    // Use a very high max_items and a short timeout to check that
    // flushing is happening based on hitting max_latency.
    let verifier = Batch::new(Ed25519Verifier::new(), 100, Duration::from_millis(500));
    timeout(Duration::from_secs(1), sign_and_verify(verifier, 10, None))
        .await
        .map_err(|e| eyre!(e))?
        .map_err(|e| eyre!(e))?;

    Ok(())
}

#[tokio::test]
async fn fallback_verification() -> Result<(), Report> {
    zebra_test::init();

    let verifier = Fallback::new(
        Batch::new(Ed25519Verifier::new(), 10, Duration::from_millis(100)),
        tower::service_fn(|item: Ed25519Item| async move { item.verify_single() }),
    );

    sign_and_verify(verifier, 100, Some(39))
        .await
        .map_err(|e| eyre!(e))?;

    Ok(())
}
