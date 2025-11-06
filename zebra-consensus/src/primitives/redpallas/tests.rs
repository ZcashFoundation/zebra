//! Tests for redpallas signature verification

#![allow(clippy::unwrap_in_result)]

use super::*;

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use tower::ServiceExt;

use zebra_chain::primitives::reddsa::{
    orchard::{Binding, SpendAuth},
    SigningKey, VerificationKey,
};

async fn sign_and_verify<V>(mut verifier: V, n: usize) -> Result<(), V::Error>
where
    V: Service<Item, Response = ()>,
{
    let mut rng = thread_rng();
    let mut results = FuturesUnordered::new();
    for i in 0..n {
        let span = tracing::trace_span!("sig", i);
        let msg = b"BatchVerifyTest";

        match i % 2 {
            0 => {
                let sk = SigningKey::<SpendAuth>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let sig = sk.sign(&mut rng, &msg[..]);
                verifier.ready().await?;
                results.push(
                    span.in_scope(|| verifier.call(Item::from_spendauth(vk.into(), sig, msg))),
                )
            }
            1 => {
                let sk = SigningKey::<Binding>::new(&mut rng);
                let vk = VerificationKey::from(&sk);
                let sig = sk.sign(&mut rng, &msg[..]);
                verifier.ready().await?;
                results
                    .push(span.in_scope(|| verifier.call(Item::from_binding(vk.into(), sig, msg))))
            }
            _ => panic!(),
        }
    }

    while let Some(result) = results.next().await {
        result?;
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
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
