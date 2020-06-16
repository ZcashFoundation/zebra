use std::{
    future::Future,
    pin::Pin,
    sync::Once,
    task::{Context, Poll},
    time::Duration,
};

use ed25519_zebra::*;
use futures::stream::{FuturesUnordered, StreamExt};
use rand::thread_rng;
use tokio::sync::broadcast::{channel, RecvError, Sender};
use tower::{Service, ServiceExt};
use tower_batch::{Batch, BatchControl};

// ============ service impl ============

pub struct Ed25519Verifier {
    batch: batch::Verifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

impl Ed25519Verifier {
    pub fn new() -> Self {
        let batch = batch::Verifier::default();
        // XXX(hdevalence) what's a reasonable choice here?
        let (tx, _) = channel(10);
        Self { tx, batch }
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
                let batch = std::mem::replace(&mut self.batch, batch::Verifier::default());
                let _ = self.tx.send(batch.verify(thread_rng()));
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Ed25519Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = std::mem::replace(&mut self.batch, batch::Verifier::default());
        let _ = self.tx.send(batch.verify(thread_rng()));
    }
}

// =============== testing code ========

static LOGGER_INIT: Once = Once::new();

fn install_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    LOGGER_INIT.call_once(|| {
        let fmt_layer = fmt::layer().with_target(false);
        let filter_layer = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();
    })
}

async fn sign_and_verify<V>(mut verifier: V, n: usize)
where
    V: Service<Ed25519Item, Response = ()>,
    <V as Service<Ed25519Item>>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    let mut results = FuturesUnordered::new();
    for i in 0..n {
        let span = tracing::trace_span!("sig", i);
        let sk = SigningKey::new(thread_rng());
        let vk_bytes = VerificationKeyBytes::from(&sk);
        let msg = b"BatchVerifyTest";
        let sig = sk.sign(&msg[..]);

        verifier.ready_and().await.map_err(|e| e.into()).unwrap();
        results.push(span.in_scope(|| verifier.call((vk_bytes, sig, msg).into())))
    }

    while let Some(result) = results.next().await {
        let result = result.map_err(|e| e.into());
        tracing::trace!(?result);
        assert!(result.is_ok());
    }
}

#[tokio::test]
async fn batch_flushes_on_max_items() {
    use tokio::time::timeout;
    install_tracing();

    // Use a very long max_latency and a short timeout to check that
    // flushing is happening based on hitting max_items.
    let verifier = Batch::new(Ed25519Verifier::new(), 10, Duration::from_secs(1000));
    assert!(
        timeout(Duration::from_secs(1), sign_and_verify(verifier, 100))
            .await
            .is_ok()
    )
}

#[tokio::test]
async fn batch_flushes_on_max_latency() {
    use tokio::time::timeout;
    install_tracing();

    // Use a very high max_items and a short timeout to check that
    // flushing is happening based on hitting max_latency.
    let verifier = Batch::new(Ed25519Verifier::new(), 100, Duration::from_millis(500));
    assert!(
        timeout(Duration::from_secs(1), sign_and_verify(verifier, 10))
            .await
            .is_ok()
    )
}
