//! Tests for Ed25519 signature verification

#![allow(clippy::unwrap_in_result)]

use std::time::Duration;

use color_eyre::eyre::{eyre, Report, Result};
use futures::stream::{FuturesOrdered, StreamExt};
use tower::ServiceExt;

use crate::primitives::ed25519::*;

async fn sign_and_verify<V>(
    mut verifier: V,
    n: usize,
    bad_index: Option<usize>,
) -> Result<(), V::Error>
where
    V: Service<Item, Response = ()>,
{
    let mut results = FuturesOrdered::new();
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

        verifier.ready().await?;
        results.push_back(span.in_scope(|| verifier.call((vk_bytes, sig, msg).into())))
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

#[tokio::test(flavor = "multi_thread")]
async fn batch_flushes_on_max_items() -> Result<(), Report> {
    use tokio::time::timeout;
    let _init_guard = zebra_test::init();

    // Use a very long max_latency and a short timeout to check that
    // flushing is happening based on hitting max_items.
    //
    // Create our own verifier, so we don't shut down a shared verifier used by other tests.
    let verifier = Batch::new(Verifier::default(), 10, 5, Duration::from_secs(1000));
    timeout(Duration::from_secs(5), sign_and_verify(verifier, 100, None))
        .await
        .map_err(|e| eyre!(e))?
        .map_err(|e| eyre!(e))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_flushes_on_max_latency() -> Result<(), Report> {
    use tokio::time::timeout;
    let _init_guard = zebra_test::init();

    // Use a very high max_items and a short timeout to check that
    // flushing is happening based on hitting max_latency.
    //
    // Create our own verifier, so we don't shut down a shared verifier used by other tests.
    let verifier = Batch::new(Verifier::default(), 100, 10, Duration::from_millis(500));
    timeout(Duration::from_secs(5), sign_and_verify(verifier, 10, None))
        .await
        .map_err(|e| eyre!(e))?
        .map_err(|e| eyre!(e))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_verification() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Create our own verifier, so we don't shut down a shared verifier used by other tests.
    let verifier = Fallback::new(
        Batch::new(Verifier::default(), 10, 1, Duration::from_millis(100)),
        tower::service_fn(|item: Item| async move { item.verify_single() }),
    );

    sign_and_verify(verifier, 100, Some(39))
        .await
        .map_err(|e| eyre!(e))?;

    Ok(())
}
