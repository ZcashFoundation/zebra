//! Async Sapling batch verifier service
//!
//! Cache misses are distributed across bounded batch workers. Both bundle preparation and
//! proof/signature validation run on blocking threads, leaving asynchronous workers free.

use core::fmt;
use std::{
    future::Future,
    mem,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
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

impl RequestWeight for Item {
    fn request_weight(&self) -> usize {
        self.bundle
            .shielded_spends()
            .len()
            .saturating_add(self.bundle.shielded_outputs().len())
    }
}

/// A service that verifies Sapling shielded data in batches.
///
/// Handles batching incoming requests, driving batches to completion, and reporting results.
#[derive(Default)]
pub struct Verifier {
    /// Bundles awaiting preparation and validation on a blocking worker.
    batch: Vec<Item>,

    /// A channel for broadcasting the verification result of the batch.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: watch::Sender<Option<BatchResult>>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verifier")
            .field("batch", &"..")
            .field("tx", &self.tx)
            .finish()
    }
}

/// Per-bundle consensus checks and the shared proof/signature verification result.
#[derive(Debug)]
struct BatchResult {
    checks: Vec<bool>,
    valid: bool,
}

/// Prepares and validates one bounded batch, entirely off the asynchronous workers.
fn validate(items: Vec<Item>) -> BatchResult {
    let mut batch = BatchValidator::default();
    let checks = items
        .into_iter()
        .map(|item| batch.check_bundle(item.bundle, item.sighash.into()))
        .collect();
    let (spend_vk, output_vk) = SAPLING.verifying_keys();
    // A failed check can leave proofs or signatures in the batch. Finish that work before
    // returning any item, but do not send unrelated successful checks through fallback.
    let valid = batch.validate(&spend_vk, &output_vk, rand::rng());
    BatchResult { checks, valid }
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
            let _ = tx.send(Some(validate(batch)));
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

                let index = self.batch.len();
                self.batch.push(item);

                async move {
                    rx.changed()
                        .await
                        .map_err(|_| BoxError::from("verifier was dropped without flushing"))?;
                    let result = rx.borrow();
                    let result = result.as_ref().ok_or_else(|| {
                        BoxError::from("threadpool unexpectedly dropped channel sender")
                    })?;
                    if !result.checks[index] {
                        return Err(TransactionError::SaplingVerificationFailed.into());
                    }

                    // We use a new channel for each batch, so we always get the correct
                    // batch result here.
                    if result.valid {
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
                    let spawn_result = tokio::task::spawn_blocking(move || validate(batch)).await;
                    let duration = start.elapsed().as_secs_f64();

                    let result_label = match &spawn_result {
                        Ok(result) if result.valid => "success",
                        _ => "failure",
                    };
                    metrics::histogram!(
                        "zebra.consensus.batch.duration_seconds",
                        "verifier" => "groth16_sapling",
                        "result" => result_label
                    )
                    .record(duration);

                    let result = spawn_result.map_err(Self::Error::from)?;
                    let _ = tx.send(Some(result));
                    Ok(())
                }
                .boxed()
            }
        }
    }
}

/// Verifies a single [`Item`].
///
/// Both bundle preparation and proof/signature validation use the configured Rayon pool.
pub fn verify_single(
    item: Item,
) -> Pin<Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send>> {
    async move {
        let is_valid = super::spawn_fifo(move || {
            // Unlike the shared verifier, a single-item batch must not spawn work from Drop.
            let mut batch = BatchValidator::default();
            if !batch.check_bundle(item.bundle, item.sighash.into()) {
                return false;
            }
            let (spend_vk, output_vk) = SAPLING.verifying_keys();
            batch.validate(&spend_vk, &output_vk, rand::rng())
        })
        .await?;

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
    ShardedVerifier,
    ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), Box<dyn std::error::Error + Send + Sync>>>>,
>;

/// Round-robin admission to independent, bounded Sapling batch workers.
#[derive(Clone)]
pub(crate) struct ShardedVerifier {
    shards: Arc<[Batch<Verifier, Item>]>,
    next: Arc<AtomicUsize>,
    ready: Option<Batch<Verifier, Item>>,
}

impl ShardedVerifier {
    fn new(shard_count: usize) -> Self {
        // Divide the existing concurrency budget rather than multiplying it by the shard count.
        let max_batches = (rayon::current_num_threads() / shard_count).max(1);
        let shards = (0..shard_count)
            .map(|_| {
                Batch::new(
                    Verifier::default(),
                    super::MAX_BATCH_SIZE,
                    max_batches,
                    super::MAX_BATCH_LATENCY,
                    super::MAX_BATCH_IDLE,
                )
            })
            .collect();
        Self {
            shards,
            next: Arc::new(AtomicUsize::new(0)),
            ready: None,
        }
    }

    /// Flushes every shard after its already-admitted items, including partial batches.
    pub(super) fn try_flush(&self) -> Result<bool, BoxError> {
        let mut result = Ok(true);
        for shard in self.shards.iter() {
            // A failed or full shard must not prevent flushing the remaining shards.
            let flushed = shard.clone().try_flush();
            result = result.and_then(|all| flushed.map(|flushed| all && flushed));
        }
        result
    }
}

impl Service<Item> for ShardedVerifier {
    type Response = ();
    type Error = BoxError;
    type Future = <Batch<Verifier, Item> as Service<Item>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let shard = self.ready.get_or_insert_with(|| {
            let index = self.next.fetch_add(1, Ordering::Relaxed) % self.shards.len();
            self.shards[index].clone()
        });
        shard.poll_ready(cx)
    }

    fn call(&mut self, item: Item) -> Self::Future {
        self.ready
            .take()
            .expect("poll_ready reserves a shard before call")
            .call(item)
    }
}

/// The concrete type of the global Sapling verification service.
pub(crate) type VerifierService = Cached<BatchFallbackService>;

/// Global batch verification context for Sapling shielded data.
///
/// The stack is wrapped in a [`Cached`] so that a bundle verified when its transaction was
/// gossiped into the mempool does not have to be verified again when the block that mines it
/// arrives. One cache covers all of Sapling: its spend and output verifying keys have never
/// changed, so unlike Orchard there are no circuit eras to keep apart.
pub(crate) static VERIFIER: Lazy<VerifierService> = Lazy::new(|| {
    Cached::new(batch_fallback_verifier(), CACHE_CAPACITY, "groth16_sapling")
        .with_flush(super::flush::Verifier::Sapling)
});

/// Queues a flush without bypassing the batch worker's queue or concurrency bounds.
pub(super) fn try_flush() -> Result<bool, BoxError> {
    VERIFIER.inner().primary().try_flush()
}

/// Returns how many times `item` reached this verifier's inner service.
#[cfg(test)]
pub(crate) fn inner_calls_for(verifier: &VerifierService, item: &Item) -> usize {
    verifier.inner_calls_for(item)
}

/// Chooses shards within the configured Rayon CPU budget, including single-threaded nodes.
fn shard_count(parallelism: usize) -> usize {
    (parallelism / 4).clamp(1, 8)
}

/// Builds the uncached batching-and-fallback stack.
pub(super) fn batch_fallback_verifier() -> BatchFallbackService {
    Fallback::new(
        ShardedVerifier::new(shard_count(rayon::current_num_threads())),
        tower::service_fn(verify_single as fn(Item) -> _),
    )
}
