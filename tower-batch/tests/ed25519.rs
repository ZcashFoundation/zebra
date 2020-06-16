use std::{
    convert::TryFrom,
    future::Future,
    pin::Pin,
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
    batch: BatchVerifier,
    // This uses a "broadcast" channel, which is an mpmc channel. Tokio also
    // provides a spmc channel, "watch", but it only keeps the latest value, so
    // using it would require thinking through whether it was possible for
    // results from one batch to be mixed with another.
    tx: Sender<Result<(), Error>>,
}

impl Ed25519Verifier {
    pub fn new() -> Self {
        let batch = BatchVerifier::default();
        let (tx, _) = channel(1);
        Self { tx, batch }
    }
}

type Request<'msg> = (VerificationKeyBytes, Signature, &'msg [u8]);

impl<'msg> Service<BatchControl<Request<'msg>>> for Ed25519Verifier {
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Request<'msg>>) -> Self::Future {
        match req {
            BatchControl::Item((vk_bytes, sig, msg)) => {
                self.batch.queue(vk_bytes, sig, msg);
                let mut rx = self.tx.subscribe();
                Box::pin(async move {
                    match rx.recv().await {
                        Ok(result) => result,
                        // this would be bad
                        Err(RecvError::Lagged(_)) => Err(Error::InvalidSignature),
                        Err(RecvError::Closed) => panic!("verifier was dropped without flushing"),
                    }
                })
            }
            BatchControl::Flush => {
                let batch = std::mem::replace(&mut self.batch, BatchVerifier::default());
                let _ = self.tx.send(batch.verify(thread_rng()));
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Ed25519Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = std::mem::replace(&mut self.batch, BatchVerifier::default());
        let _ = self.tx.send(batch.verify(thread_rng()));
    }
}

// =============== testing code ========

async fn sign_and_verify<V>(mut verifier: V, n: usize)
where
    for<'msg> V: Service<Request<'msg>>,
    for<'msg> <V as Service<Request<'msg>>>::Error:
        Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    let mut results = FuturesUnordered::new();
    for _ in 0..n {
        let sk = SigningKey::new(thread_rng());
        let vk_bytes = VerificationKeyBytes::from(&sk);
        let msg = b"BatchVerifyTest";
        let sig = sk.sign(&msg[..]);
        results.push(
            verifier
                .ready_and()
                .await
                .map_err(|e| e.into())
                .unwrap()
                .call((vk_bytes, sig, &msg[..])),
        )
    }

    while let Some(result) = results.next().await {
        assert!(result.is_ok());
    }
}

#[tokio::test]
async fn individual_verification_with_service_fn() {
    let verifier = tower::service_fn(|(vk_bytes, sig, msg): Request| {
        let result = VerificationKey::try_from(vk_bytes).and_then(|vk| vk.verify(&sig, msg));
        async move { result }
    });

    sign_and_verify(verifier, 100).await;
}

#[tokio::test]
async fn batch_flushes_on_max_items() {
    use tokio::time::timeout;

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

    // Use a very high max_items and a short timeout to check that
    // flushing is happening based on hitting max_latency.
    let verifier = Batch::new(Ed25519Verifier::new(), 100, Duration::from_millis(500));
    assert!(
        timeout(Duration::from_secs(1), sign_and_verify(verifier, 10))
            .await
            .is_ok()
    )
}
