//! Tests for Ed25519 signature verification

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use tower::ServiceExt;
use tower_batch_control::Batch;

use crate::primitives::ed25519::*;

async fn sign_and_verify<V>(mut verifier: V, n: usize) -> Result<(), V::Error>
where
    V: Service<Item, Response = ()>,
{
    let mut rng = thread_rng();
    let mut results = FuturesUnordered::new();
    for i in 0..n {
        let span = tracing::trace_span!("sig", i);
        let msg = b"BatchVerifyTest";

        let sk = SigningKey::new(&mut rng);
        let vk = VerificationKey::from(&sk);
        let sig = sk.sign(&msg[..]);
        verifier.ready().await?;
        results.push(span.in_scope(|| verifier.call((vk.into(), sig, msg).into())))
    }

    while let Some(result) = results.next().await {
        result?;
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_flushes_on_max_items_test() -> Result<()> {
    batch_flushes_on_max_items().await
}

#[spandoc::spandoc]
async fn batch_flushes_on_max_items() -> Result<()> {
    use tokio::time::timeout;

    // Use a very long max_latency and a short timeout to check that
    // flushing is happening based on hitting max_items.
    let verifier = Batch::new(Verifier::default(), 10, 5, Duration::from_secs(1000));
    timeout(Duration::from_secs(5), sign_and_verify(verifier, 100))
        .await?
        .map_err(|e| eyre!(e))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_flushes_on_max_latency_test() -> Result<()> {
    batch_flushes_on_max_latency().await
}

#[spandoc::spandoc]
async fn batch_flushes_on_max_latency() -> Result<()> {
    use tokio::time::timeout;

    // Use a very high max_items and a short timeout to check that
    // flushing is happening based on hitting max_latency.
    let verifier = Batch::new(Verifier::default(), 100, 10, Duration::from_millis(500));
    timeout(Duration::from_secs(5), sign_and_verify(verifier, 10))
        .await?
        .map_err(|e| eyre!(e))?;

    Ok(())
}
