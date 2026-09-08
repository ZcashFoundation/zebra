//! A bounded cache of shielded bundle verifications that have already succeeded.
//!
//! Zebra verifies a transaction's shielded proofs and signatures when the transaction arrives
//! over mempool gossip, and again when it arrives in a block. This service skips the second
//! verification. The Halo2 Orchard and Ironwood verifiers ([`super::halo2`]) and the Sapling
//! verifier ([`super::sapling`]) share it, and key their entries the same way.
//!
//! # Why this is not the mempool bypass
//!
//! Zebra once skipped whole-transaction verification for transactions already in the mempool, and
//! removed it as a security fix (PR #10494). Transaction validity depends on height, block time
//! and spent outputs, and that cache's key named none of them.
//!
//! This cache remembers successful bundle verification by transaction ID, sighash and shielded
//! pool. A hit still runs the whole transaction verifier against the block's height, time and
//! spent outputs, and skips only the proof and signature checks. Each Orchard circuit era has its
//! own cache, which binds an entry to its verifying key (see [`super::halo2::orchard_v5_verifier_for`]);
//! Sapling has one verifying key pair for all of history, so one cache covers it.
//!
//! Only `Ok` results are cached. A batch error is not per-item evidence, because
//! [`Fallback`](tower_fallback::Fallback) re-verifies failures singly, and it may not be a verdict
//! at all — a shut-down batch worker reports the same way. Caching it would reject a valid block.

use std::{
    collections::{HashSet, VecDeque},
    future,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use tower::{Service, ServiceExt};
use zebra_chain::transaction::UnminedTxId;

use crate::BoxError;

#[cfg(test)]
mod tests;

/// The number of verified-bundle keys retained per cache.
///
/// Sized to hold several blocks of history plus a full mempool, so that a transaction gossiped
/// well before the block that mines it is still remembered.
///
/// Each key is a 98-byte transaction ID, sighash and pool tag, held twice — once in the lookup
/// set and once in the eviction queue — so a full cache costs about 5 MiB, and about 20 MiB
/// across the three Orchard circuit eras and Sapling. Only the eras a node actually verifies pay
/// it, because each cache is built with its verifier on first use.
pub(super) const CACHE_CAPACITY: usize = 20_000;

/// The label naming which verifier's cache a metric belongs to.
///
/// One cache instance per Orchard circuit era and one for Sapling all report under the same
/// metric names, so every series carries this label. Its values are `halo2_pre_nu6_2`,
/// `halo2_nu6_2`, `halo2_nu6_3_onward` and `groth16_sapling`. Only Sapling's matches the
/// `verifier` label of its `zebra.consensus.batch.duration_seconds` series as well; the Halo2
/// batch metric reports all three eras as one `halo2` series.
const VERIFIER_LABEL: &str = "verifier";

/// Counts verifications answered from a cache.
const CACHE_HIT: &str = "zebra.consensus.cache.hit";

/// Counts verifications that reached a cache's inner service.
const CACHE_MISS: &str = "zebra.consensus.cache.miss";

/// Counts keys recorded as verified.
const CACHE_INSERT: &str = "zebra.consensus.cache.insert";

/// Counts keys dropped to stay within a cache's capacity.
const CACHE_EVICT: &str = "zebra.consensus.cache.evict";

/// Reports how many keys a cache currently remembers.
const CACHE_SIZE: &str = "zebra.consensus.cache.size";

/// The shielded bundle slot a cache entry was verified for.
///
/// One v6 transaction has an Orchard bundle, an Ironwood bundle and a Sapling bundle, all under
/// one transaction ID and one sighash, so the key names which one it stands for. The Orchard and
/// Ironwood caches at NU6.3 onward are the same cache, so this tag is what keeps their entries
/// apart; Sapling has its own cache and its tag is defence in depth.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ShieldedPool {
    /// The Sapling value pool.
    Sapling,

    /// The Orchard value pool.
    Orchard,

    /// The Ironwood value pool.
    Ironwood,
}

impl From<orchard::ValuePool> for ShieldedPool {
    fn from(pool: orchard::ValuePool) -> Self {
        match pool {
            orchard::ValuePool::Orchard => Self::Orchard,
            orchard::ValuePool::Ironwood => Self::Ironwood,
        }
    }
}

/// A transaction, sighash and shielded pool whose bundle has verified.
///
/// # Correctness
///
/// A hit replaces a verification, so the key must determine every input that verification reads:
/// the bundle and the sighash.
///
/// The transaction ID determines the bundle, in both of the forms it takes:
///
///   * [`UnminedTxId::Witnessed`] carries a [`WtxId`](zebra_chain::transaction::WtxId), whose
///     txid commits to the transaction's effecting data and whose ZIP 244 authorizing-data digest
///     commits to its proofs and signatures. The txid alone would not: it excludes authorizing
///     data, which is what CVE-2026-34377 exploited.
///   * [`UnminedTxId::Legacy`] is a v1-v4 transaction ID, the hash of the whole serialized
///     transaction, so it commits to the Sapling proofs and signatures directly. V4 transactions
///     have no witnessed ID, and this is why they do not need one here.
///
/// The sighash is named separately because it is not always a function of the transaction alone.
/// A v5 or v6 sighash also commits to the amounts and `scriptPubKey`s of the spent transparent
/// outputs, which the verification context supplies. A v4 shielded sighash does not — it is
/// computed with no input index, so ZIP 143 and ZIP 243 leave the spent output out — but it does
/// commit to the block's consensus branch id, which a v4 transaction ID does not carry, and which
/// selects the verification a bundle is checked against.
///
/// The verifying key is absent on purpose. Each Orchard circuit era has its own cache, so an
/// entry is only ever read back under the key it was written against, and Sapling has one key
/// pair for all of history.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct CacheKey {
    /// The ID of the transaction the bundle was parsed from.
    tx_id: UnminedTxId,

    /// The signature digest used to verify the bundle's signatures.
    sighash: [u8; 32],

    /// The bundle slot verified for this transaction.
    pool: ShieldedPool,
}

impl CacheKey {
    /// Returns the key for `pool`'s bundle in the transaction identified by `tx_id`, verified
    /// against `sighash`.
    pub(super) fn new(tx_id: UnminedTxId, sighash: [u8; 32], pool: ShieldedPool) -> Self {
        Self {
            tx_id,
            sighash,
            pool,
        }
    }
}

/// An item whose successful verification can be remembered.
///
/// Items without a key are verified normally and never cached, which is how a caller that has no
/// transaction identity to offer stays correct.
///
/// # Correctness
///
/// An implementer promises that the returned key determines every input the item's verification
/// reads: the bundle, and the sighash it is checked against. [`Cached`] answers any later item
/// with an equal key from the remembered `Ok` without verifying it, so a key that two different
/// bundles can share accepts a bundle this node never checked. An implementer that cannot offer
/// a complete key returns `None`, and the item is verified every time.
pub(super) trait CachedItem {
    /// Returns the key this item's successful verification is remembered under, or `None` if the
    /// item must always be verified.
    ///
    /// The key is a pure function of the item: two calls on one item return the same key, and two
    /// items with equal keys have identical verification inputs. [`CacheKey`] derives why a
    /// transaction ID, a sighash and a shielded pool are enough.
    fn cache_key(&self) -> Option<CacheKey>;
}

/// What one [`VerifiedBundles::insert`] did, so the caller can report it after releasing the
/// lock.
///
/// Metrics are emitted outside the critical section: a labelled `metrics` macro allocates its
/// label set on every call, and this lock is taken by every shielded verification the node runs.
#[derive(Clone, Copy, Debug)]
struct InsertOutcome {
    /// Whether this key was new, rather than a concurrent duplicate.
    inserted: bool,

    /// How many keys were evicted to make room for it.
    evicted: usize,

    /// How many keys the cache holds now.
    size: usize,
}

/// A bounded set of keys for bundles that have already verified successfully.
///
/// Eviction is first-in-first-out rather than least-recently-used: the working set is the
/// mempool, which turns over in arrival order anyway, and FIFO needs no bookkeeping on the read
/// path. Evicting an entry only costs a re-verification, never correctness.
///
/// # Correctness
///
/// `keys` and `insertion_order` hold the same keys. `keys` answers [`Self::contains`], and
/// `insertion_order` chooses which key to drop. Only [`Self::insert`] and `Self::clear` change
/// them, and each changes both, so no caller can leave them holding different keys.
#[derive(Debug)]
struct VerifiedBundles {
    /// The keys currently remembered.
    keys: HashSet<CacheKey>,

    /// The same keys in insertion order, so the oldest can be evicted.
    insertion_order: VecDeque<CacheKey>,

    /// The maximum number of keys to retain.
    capacity: usize,
}

impl VerifiedBundles {
    /// Creates an empty cache that retains at most `capacity` keys.
    fn new(capacity: usize) -> Self {
        Self {
            keys: HashSet::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Returns `true` if `key` has already verified.
    fn contains(&self, key: &CacheKey) -> bool {
        self.keys.contains(key)
    }

    /// Records that `key` has verified, evicting the oldest keys to stay within the capacity.
    fn insert(&mut self, key: CacheKey) -> InsertOutcome {
        // Concurrent verifications of the same item both miss and both insert. The second is a
        // no-op, and must not push a duplicate into the eviction queue.
        if !self.keys.insert(key) {
            return InsertOutcome {
                inserted: false,
                evicted: 0,
                size: self.keys.len(),
            };
        }

        // Evict before pushing, so the queue never has to grow past the capacity it was built
        // with. Pushing first would take it to `capacity + 1` and double its allocation for the
        // rest of the process.
        let mut evicted = 0;
        while self.insertion_order.len() >= self.capacity {
            // The queue is non-empty whenever its length reaches a capacity of one or more, which
            // is what every caller passes. Breaking rather than unwrapping keeps a capacity of
            // zero from looping forever.
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.keys.remove(&oldest);
            evicted += 1;
        }

        self.insertion_order.push_back(key);

        InsertOutcome {
            inserted: true,
            evicted,
            size: self.keys.len(),
        }
    }

    /// Forgets every key.
    ///
    /// Test-only: it lets a test start from a cold cache. Forgetting a key only costs a
    /// re-verification, so it is always safe.
    #[cfg(test)]
    fn clear(&mut self) {
        self.keys.clear();
        self.insertion_order.clear();
    }
}

impl InsertOutcome {
    /// Reports this insert under `verifier_name`.
    ///
    /// [`Cached::call`] calls this after it releases the cache lock.
    fn report(self, verifier_name: &'static str) {
        if !self.inserted {
            return;
        }

        metrics::counter!(CACHE_INSERT, VERIFIER_LABEL => verifier_name).increment(1);

        if self.evicted > 0 {
            // Cast is safe: at most `capacity` keys are evicted by one insert.
            metrics::counter!(CACHE_EVICT, VERIFIER_LABEL => verifier_name)
                .increment(self.evicted as u64);
        }

        // Cast is safe: the length is bounded by `capacity`, far below f64's exact integer range.
        metrics::gauge!(CACHE_SIZE, VERIFIER_LABEL => verifier_name).set(self.size as f64);
    }
}

/// A service that skips inner verification for items whose bundle has already verified.
///
/// This wraps one verifier's batch-and-fallback stack. The cache is shared between clones, so
/// every handle to a global verifier sees the same set of verified bundles.
///
/// This type is public only because it appears in existing public verifier signatures. The
/// private `cache` module is not re-exported, and its constructor and accessors are private.
pub struct Cached<S> {
    /// The verification service to consult on a miss.
    inner: S,

    /// The keys of items that have already verified under this cache's verifying key.
    verified: Arc<Mutex<VerifiedBundles>>,

    /// The value this cache reports in the `verifier` label of its metrics.
    verifier_name: &'static str,

    /// The keys of the items that reached the inner service, in call order.
    ///
    /// Test-only. See [`Self::inner_calls_for`].
    #[cfg(test)]
    inner_calls: Arc<Mutex<Vec<CacheKey>>>,
}

impl<S: Clone> Clone for Cached<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            verified: self.verified.clone(),
            verifier_name: self.verifier_name,
            #[cfg(test)]
            inner_calls: self.inner_calls.clone(),
        }
    }
}

impl<S> Cached<S> {
    /// Wraps `inner` in a cache that retains at most `capacity` verified-bundle keys and reports
    /// its metrics under the `verifier` label `verifier_name`.
    pub(super) fn new(inner: S, capacity: usize, verifier_name: &'static str) -> Self {
        Self {
            inner,
            verified: Arc::new(Mutex::new(VerifiedBundles::new(capacity))),
            verifier_name,
            #[cfg(test)]
            inner_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns the wrapped verification service.
    ///
    /// Test-only: it lets a test read back the inner service it installed with
    /// [`Self::with_inner`].
    #[cfg(test)]
    pub(super) fn inner(&self) -> &S {
        &self.inner
    }

    /// Returns how many times an item equivalent to `item` has reached the inner service.
    ///
    /// Test-only. It counts one item rather than all calls because the global verifiers are shared
    /// by every test in the process, so a plain call counter would also see verifications a test
    /// did not make. Counting one transaction's own item isolates a test from the rest.
    ///
    /// Readiness failures are not counted: an item that never reached the inner service was never
    /// verified by it.
    #[cfg(test)]
    pub(super) fn inner_calls_for<I: CachedItem>(&self, item: &I) -> usize {
        let Some(key) = item.cache_key() else {
            return 0;
        };

        self.inner_calls
            .lock()
            .expect("inner call record mutex should not be poisoned")
            .iter()
            .filter(|called| **called == key)
            .count()
    }

    /// Returns a cache sharing this one's remembered keys, but consulting `inner` on a miss.
    ///
    /// Test-only. It exists so a test can warm the cache through a healthy service and then swap
    /// in a broken one, which is how the hit path is exercised in isolation from the inner
    /// service.
    #[cfg(test)]
    pub(super) fn with_inner<T>(&self, inner: T) -> Cached<T> {
        Cached {
            inner,
            verified: self.verified.clone(),
            verifier_name: self.verifier_name,
            inner_calls: self.inner_calls.clone(),
        }
    }
}

impl<S, I> Service<I> for Cached<S>
where
    // `Send + 'static` because a miss moves the item into the boxed future that awaits inner
    // readiness — see `poll_ready`.
    I: CachedItem + Send + 'static,
    S: Service<I, Response = (), Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = ();
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<(), BoxError>>;

    /// Always ready.
    ///
    /// This does not delegate to the inner service, because the item is not known yet, so neither
    /// is whether the inner service will be used at all. Delegating would reserve inner capacity
    /// for every request, including the hits that never spend it:
    ///
    ///   * [`Batch::poll_ready`](tower_batch_control::Batch) holds a semaphore permit until
    ///     `Batch::call` consumes it. A hit never calls, so it holds that permit until the handle
    ///     drops, denying capacity to a genuine miss.
    ///   * `Batch::poll_ready` also errors once its worker exits. Callers poll before they call,
    ///     so that error would surface for an item this cache already holds, reporting a verified
    ///     proof as a verification failure.
    ///
    /// [`Self::call`] awaits readiness on the miss path instead. The semaphore still bounds
    /// concurrent batch requests; only the timing of the wait changes.
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, item: I) -> Self::Future {
        // Copied once here, outside `Fallback`, which clones every request eagerly.
        let key = item.cache_key();

        if let Some(key) = key {
            if self
                .verified
                .lock()
                .expect("verified bundle cache mutex should not be poisoned")
                .contains(&key)
            {
                metrics::counter!(CACHE_HIT, VERIFIER_LABEL => self.verifier_name).increment(1);
                return future::ready(Ok(())).boxed();
            }
        }

        metrics::counter!(CACHE_MISS, VERIFIER_LABEL => self.verifier_name).increment(1);

        let verified = self.verified.clone();
        let verifier_name = self.verifier_name;
        let mut inner = self.inner.clone();

        #[cfg(test)]
        let inner_calls = self.inner_calls.clone();

        async move {
            // Readiness is acquired here rather than in `poll_ready` so that only misses reserve
            // inner capacity. See `poll_ready`.
            let result = match inner.ready().await {
                Ok(inner) => {
                    #[cfg(test)]
                    if let Some(key) = key {
                        inner_calls
                            .lock()
                            .expect("inner call record mutex should not be poisoned")
                            .push(key);
                    }

                    inner.call(item).await
                }
                Err(error) => Err(error),
            };

            // Only successes are recorded: see the module docs.
            if let (Ok(()), Some(key)) = (&result, key) {
                let outcome = verified
                    .lock()
                    .expect("verified bundle cache mutex should not be poisoned")
                    .insert(key);

                outcome.report(verifier_name);
            }

            result
        }
        .boxed()
    }
}
