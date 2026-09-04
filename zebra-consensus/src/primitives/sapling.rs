//! Async Sapling batch verifier service

use core::fmt;
use std::{
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
use rand::thread_rng;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use tower_fallback::Fallback;

use sapling_crypto::{bundle::Authorized, BatchValidator, Bundle};
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::value::ZatBalance;
use zebra_chain::transaction::{SigHash, UnminedTxId};

use crate::{error::TransactionError, BoxError};

use super::cache::{CacheKey, Cached, CachedItem, ShieldedPool, CACHE_CAPACITY};

#[cfg(test)]
mod tests;

/// Sapling prover containing spend and output params for the Sapling circuit.
///
/// Used to:
///
/// - construct Sapling outputs in coinbase txs, and
/// - verify Sapling shielded data in the tx verifier.
static SAPLING: Lazy<LocalTxProver> = Lazy::new(LocalTxProver::bundled);

/// Returns the shared Sapling prover.
///
/// Parsing the bundled Sapling parameters takes time, so callers that build Sapling
/// outputs share this prover instead of parsing the parameters again.
pub fn prover() -> &'static LocalTxProver {
    &SAPLING
}

/// A Sapling verification item, used as the request type of the service.
///
/// Every item carries the cache key its successful verification is remembered under, derived from
/// its transaction's ID and sighash.
#[derive(Clone)]
pub struct Item {
    /// The bundle containing the Sapling shielded data to verify.
    bundle: Bundle<Authorized, ZatBalance>,
    /// The sighash of the transaction that contains the Sapling shielded data.
    sighash: SigHash,
    /// The key this item's successful verification is remembered under.
    cache_key: CacheKey,
}

impl Item {
    /// Creates a new [`Item`] from a Sapling bundle, its sighash, and its transaction's ID.
    ///
    /// `tx_id` must identify the transaction containing `bundle`, because the cache treats it as
    /// determining the bundle — see this type's [`CachedItem`] implementation. The transaction
    /// verifier passes the ID it derived from its own request, whose caller must preserve this
    /// invariant.
    pub fn new(
        bundle: Bundle<Authorized, ZatBalance>,
        sighash: SigHash,
        tx_id: UnminedTxId,
    ) -> Self {
        Self {
            bundle,
            sighash,
            cache_key: CacheKey::new(tx_id, sighash.0, ShieldedPool::Sapling),
        }
    }
}

impl CachedItem for Item {
    /// Returns the key this item's successful verification is remembered under.
    ///
    /// The transaction ID commits to the bundle, in both of the forms it takes. A v5 or v6
    /// transaction's [`WtxId`](zebra_chain::transaction::WtxId) pairs a txid over the effecting
    /// data with a ZIP 244 authorizing-data digest over the proofs and signatures; a v4
    /// transaction's legacy ID is the hash of its whole serialization, which contains the same
    /// authorizing data. So unlike Orchard, which only exists in v5 and v6 transactions, Sapling
    /// caches v4 bundles too.
    ///
    /// The sighash is keyed separately because it is not always a function of the transaction
    /// alone. A v5 or v6 sighash also commits to the amounts and scripts of the spent transparent
    /// outputs, which the verification context supplies. A v4 shielded sighash does not — it is
    /// computed with no input index, so ZIP 143 and ZIP 243 leave the spent output out — but it
    /// does commit to the consensus branch id of the block, which the transaction ID of a v4
    /// transaction does not carry.
    ///
    /// The verifying keys are absent on purpose: Sapling has one spend and one output verifying
    /// key for all of history, so unlike Orchard it has no circuit eras to keep apart, and every
    /// entry in this cache was written under the same keys it is read back under.
    fn cache_key(&self) -> Option<CacheKey> {
        Some(self.cache_key)
    }
}

impl RequestWeight for Item {}

/// A service that verifies Sapling shielded data in batches.
///
/// Handles batching incoming requests, driving batches to completion, and reporting results.
#[derive(Default)]
pub struct Verifier {
    /// A batch verifier for Sapling shielded data.
    batch: BatchValidator,

    /// A channel for broadcasting the verification result of the batch.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: watch::Sender<Option<bool>>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verifier")
            .field("batch", &"..")
            .field("tx", &self.tx)
            .finish()
    }
}

impl Drop for Verifier {
    // Flush the current batch in case there are still any pending futures.
    //
    // Flushing the batch means we need to validate it. This function fires off the validation and
    // returns immediately, usually before the validation finishes.
    fn drop(&mut self) {
        let batch = mem::take(&mut self.batch);
        let tx = mem::take(&mut self.tx);

        // The validation is CPU-intensive; do it on a dedicated thread so it does not block.
        rayon::spawn_fifo(move || {
            let (spend_vk, output_vk) = SAPLING.verifying_keys();

            // Validate the batch and send the result through the channel.
            let res = batch.validate(&spend_vk, &output_vk, thread_rng());
            let _ = tx.send(Some(res));
        });
    }
}

impl Service<BatchControl<Item>> for Verifier {
    type Response = ();
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Item>) -> Self::Future {
        match req {
            BatchControl::Item(item) => {
                let mut rx = self.tx.subscribe();

                let bundle_check = self
                    .batch
                    .check_bundle(item.bundle, item.sighash.into())
                    .then_some(())
                    .ok_or(TransactionError::SaplingVerificationFailed);

                async move {
                    bundle_check.map_err(BoxError::from)?;

                    rx.changed()
                        .await
                        .map_err(|_| BoxError::from("verifier was dropped without flushing"))?;

                    // We use a new channel for each batch, so we always get the correct
                    // batch result here.
                    let is_valid = rx.borrow().ok_or_else(|| {
                        BoxError::from("threadpool unexpectedly dropped channel sender")
                    })?;

                    if is_valid {
                        metrics::counter!("proofs.sapling.verified").increment(1);
                        Ok(())
                    } else {
                        metrics::counter!("proofs.sapling.invalid").increment(1);
                        Err(BoxError::from(TransactionError::SaplingVerificationFailed))
                    }
                }
                .boxed()
            }

            BatchControl::Flush => {
                let batch = mem::take(&mut self.batch);
                let tx = mem::take(&mut self.tx);

                async move {
                    let start = std::time::Instant::now();
                    let spawn_result = tokio::task::spawn_blocking(move || {
                        let (spend_vk, output_vk) = SAPLING.verifying_keys();
                        batch.validate(&spend_vk, &output_vk, thread_rng())
                    })
                    .await;
                    let duration = start.elapsed().as_secs_f64();

                    let result_label = match &spawn_result {
                        Ok(true) => "success",
                        _ => "failure",
                    };
                    metrics::histogram!(
                        "zebra.consensus.batch.duration_seconds",
                        "verifier" => "groth16_sapling",
                        "result" => result_label
                    )
                    .record(duration);

                    // Extract the value before consuming spawn_result
                    let is_valid = spawn_result.as_ref().ok().copied();
                    let _ = tx.send(is_valid);
                    spawn_result.map(|_| ()).map_err(Self::Error::from)
                }
                .boxed()
            }
        }
    }
}

/// Verifies a single [`Item`].
pub fn verify_single(
    item: Item,
) -> Pin<Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send>> {
    async move {
        let mut verifier = Verifier::default();

        let check = verifier
            .batch
            .check_bundle(item.bundle, item.sighash.into())
            .then_some(())
            .ok_or(TransactionError::SaplingVerificationFailed);
        check.map_err(BoxError::from)?;

        let is_valid = tokio::task::spawn_blocking(move || {
            let (spend_vk, output_vk) = SAPLING.verifying_keys();

            mem::take(&mut verifier.batch).validate(&spend_vk, &output_vk, thread_rng())
        })
        .await
        .map_err(|_| BoxError::from("Sapling bundle validation thread panicked"))?;

        if is_valid {
            Ok(())
        } else {
            Err(BoxError::from(TransactionError::SaplingVerificationFailed))
        }
    }
    .boxed()
}

/// The batching-and-fallback stack for Sapling bundle verification, before caching.
type BatchFallbackService = Fallback<
    Batch<Verifier, Item>,
    ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), Box<dyn std::error::Error + Send + Sync>>>>,
>;

/// The concrete type of the global Sapling verification service.
pub type VerifierService = Cached<BatchFallbackService>;

/// Global batch verification context for Sapling shielded data.
///
/// The stack is wrapped in a [`Cached`] so that a bundle verified when its transaction was
/// gossiped into the mempool does not have to be verified again when the block that mines it
/// arrives. One cache covers all of Sapling: its spend and output verifying keys have never
/// changed, so unlike Orchard there are no circuit eras to keep apart.
pub static VERIFIER: Lazy<VerifierService> =
    Lazy::new(|| Cached::new(batch_fallback_verifier(), CACHE_CAPACITY, "groth16_sapling"));

/// Returns how many times `item` has reached the inner Sapling verifier.
///
/// Test-only. See [`Cached::inner_calls_for`].
#[cfg(test)]
pub(crate) fn inner_calls_for(item: &Item) -> usize {
    VERIFIER.inner_calls_for(item)
}

/// Builds the uncached batching-and-fallback stack.
fn batch_fallback_verifier() -> BatchFallbackService {
    Fallback::new(
        Batch::new(
            Verifier::default(),
            super::MAX_BATCH_SIZE,
            None,
            super::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(verify_single as fn(Item) -> _),
    )
}
