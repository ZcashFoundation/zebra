//! Tests for the Halo2 Orchard Action verifier.
//!
//! The key correctness property of this module is the **era split**: the Orchard Action circuit
//! (and therefore its verifying key) changes across protocol eras — it was fixed at NU6.2 for a
//! variable-base scalar-multiplication soundness bug (GHSA-jfw5-j458-pfv6) and extended again at
//! NU6.3 with the cross-address restriction. A proof produced under one circuit does not verify
//! under another era's key. These tests guard that:
//!
//!   * a real pre-NU6.2 Orchard proof verifies under the pre-NU6.2 (insecure) key, so historical
//!     blocks still re-sync;
//!   * the same proof is **rejected** by the NU6.2 (fixed) key, so the verifier is not
//!     "fail-open" — it does not accept whatever it is handed regardless of era; and
//!   * the Orchard verifier routing ([`orchard_v5_verifier_for`] / [`orchard_v6_verifier`])
//!     selects the matching key by block era — in particular a v5 Orchard bundle at NU6.3 routes
//!     to the NU6.3 cross-address key (the same key as v6 Orchard and Ironwood), not the fixed key.

use std::{
    future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use orchard::bundle::{Authorized, Bundle, BundleVersion, Flags};
use tower::{Service, ServiceExt};
use zcash_protocol::value::ZatBalance;
use zebra_chain::{
    block::Block,
    parameters::NetworkUpgrade,
    serialization::{BytesInDisplayOrder, ZcashDeserializeInto},
    transaction::{
        arbitrary::with_garbage_orchard_authorization, AuthDigest, Hash, HashType, SigHash,
        Transaction, WtxId,
    },
    transparent,
};

use crate::{error::TransactionError, BoxError};

use super::{
    orchard_v5_verifier_for, orchard_v6_verifier, CacheKey, Cached, CachedItem, Item,
    VERIFIER_NU6_2, VERIFIER_NU6_3_ONWARD, VERIFIER_PRE_NU6_2, VERIFYING_KEY_NU6_2,
    VERIFYING_KEY_PRE_NU6_2,
};

/// The `verifier` label the test caches report their metrics under.
///
/// Test caches use their own label so their counts never land in the series the production
/// verifiers report.
const TEST_CACHE_VERIFIER_LABEL: &str = "halo2_test";

/// Returns one real pre-NU6.2 Orchard bundle and its sighash, extracted from the mainnet test
/// blocks.
///
/// These mainnet blocks are NU5-era Orchard history, mined long before NU6.2, so their proofs
/// were produced by the historical (insecure) circuit and only verify under
/// [`VERIFYING_KEY_PRE_NU6_2`]. Transactions with transparent inputs are skipped because their
/// sighash needs the previous outputs they spend, which are not in the test vectors.
fn pre_nu6_2_bundle_and_sighash() -> (Bundle<Authorized, ZatBalance>, SigHash) {
    let (_tx, bundle, sighash) = pre_nu6_2_transaction_bundle_and_sighash();
    (bundle, sighash)
}

/// Returns one real pre-NU6.2 Orchard transaction, together with its bundle and sighash.
///
/// The transaction itself is needed by the cache-key tests, which derive the key from its
/// witnessed transaction ID. See [`pre_nu6_2_bundle_and_sighash`] for how it is selected.
fn pre_nu6_2_transaction_bundle_and_sighash(
) -> (Transaction, Bundle<Authorized, ZatBalance>, SigHash) {
    for bytes in zebra_test::vectors::MAINNET_BLOCKS.values() {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("hard-coded test vector must deserialize");

        for tx in &block.transactions {
            if !tx.has_orchard_shielded_data() || !tx.inputs().is_empty() {
                continue;
            }

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());
            let Ok(sighasher) = tx.sighasher(NetworkUpgrade::Nu5, all_previous_outputs) else {
                continue;
            };
            let Some(bundle) = sighasher.orchard_bundle() else {
                continue;
            };

            let sighash = sighasher.sighash(HashType::ALL, None);
            return (tx.as_ref().clone(), bundle, sighash);
        }
    }

    panic!("mainnet test blocks must contain a transparent-input-free Orchard transaction");
}

/// A real pre-NU6.2 Orchard proof verifies under the pre-NU6.2 key and is rejected by the
/// post-NU6.2 key.
///
/// This is the core guard for the era split: it proves the two keys are genuinely different and
/// that selecting the wrong era's key causes a hard verification failure. If the verifier ever
/// "fails open" (e.g. validates everything against a single key, like the rejected zcashd WIP
/// shortcut), the wrong-key assertion below would fail.
#[test]
fn pre_nu6_2_proof_only_verifies_under_pre_nu6_2_key() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();

    // Correct era key: the historical proof must verify, so pre-NU6.2 history still re-syncs.
    assert!(
        Item::new(bundle.clone(), sighash).verify_single(&VERIFYING_KEY_PRE_NU6_2),
        "a real pre-NU6.2 Orchard proof must verify under the pre-NU6.2 (insecure) key"
    );

    // Wrong era key: the same proof must be rejected. This is the not-fail-open guarantee.
    assert!(
        !Item::new(bundle, sighash).verify_single(&VERIFYING_KEY_NU6_2),
        "a pre-NU6.2 Orchard proof must be REJECTED by the post-NU6.2 (fixed) key; \
         verifying it would mean the era selection is fail-open"
    );
}

/// The Orchard verifier routing selects the correct key by block era (network upgrade).
///
/// We compare service identity by pointer: the routing functions return a borrow of one of the
/// three global `Lazy` services, so routing to the wrong service is exactly routing to the wrong
/// key. The key correctness property guarded here is that the Orchard verifying key is a function
/// of the block era, NOT the transaction version: a **v5** Orchard bundle at NU6.3 routes to the
/// NU6.3 cross-address key — the same key as v6 Orchard and Ironwood — because the cross-address
/// restriction applies to every Orchard Action from NU6.3 onward regardless of transaction version
/// (ZIP 229), so it cannot be bypassed with a v5 transaction. Only NU6.2 uses the fixed key.
///
/// This is an async test because forcing the global `Lazy` verifiers builds their `Batch` layer,
/// which spawns a worker task and therefore needs a Tokio runtime.
#[tokio::test(flavor = "multi_thread")]
async fn orchard_verifier_routing_selects_the_correct_key() {
    let pre: &'static super::VerifierService = &VERIFIER_PRE_NU6_2;
    let post: &'static super::VerifierService = &VERIFIER_NU6_2;
    let post_nu6_3: &'static super::VerifierService = &VERIFIER_NU6_3_ONWARD;

    // v5 Orchard bundles before NU6.2 (incl. upgrades from before Orchard existed) route to the
    // insecure key, the only key pre-NU6.2 Orchard history verifies under.
    for nu in [
        NetworkUpgrade::Nu5,
        NetworkUpgrade::Nu6,
        NetworkUpgrade::Nu6_1,
    ] {
        assert!(
            std::ptr::eq(orchard_v5_verifier_for(nu), pre),
            "v5 Orchard at {nu:?} must route to the pre-NU6.2 (insecure) verifier"
        );
    }

    // NU6.2 — and only NU6.2 — uses the fixed key: it is active from the NU6.2 activation height
    // until NU6.3.
    assert!(
        std::ptr::eq(orchard_v5_verifier_for(NetworkUpgrade::Nu6_2), post),
        "v5 Orchard at NU6.2 must route to the post-NU6.2 (fixed) verifier"
    );

    // v5 Orchard bundles from NU6.3 onward route to the NU6.3 (cross-address) key, NOT the fixed
    // key — the cross-address restriction cannot be bypassed with a v5 transaction. This is the
    // regression guard for the routing bug.
    for nu in [NetworkUpgrade::Nu6_3, NetworkUpgrade::Nu7] {
        assert!(
            std::ptr::eq(orchard_v5_verifier_for(nu), post_nu6_3),
            "v5 Orchard at {nu:?} must route to the NU6.3 verifier, not the post-NU6.2 fixed key"
        );
    }

    // v6 Orchard + Ironwood bundles route to the same NU6.3 (cross-address) key.
    assert!(
        std::ptr::eq(orchard_v6_verifier(), post_nu6_3),
        "v6 Orchard/Ironwood must route to the NU6.3 verifier"
    );
}

// Cache key completeness.
//
// [`Cached`] reuses a previous `Ok` for any item whose key matches, so the key must uniquely
// identify the transaction and its bundle slot.

/// Returns a deterministic witnessed transaction ID for cache behaviour tests.
fn test_wtx_id(tag: u8) -> WtxId {
    WtxId {
        id: Hash::from_bytes_in_display_order(&[tag; 32]),
        auth_digest: AuthDigest::from_bytes_in_display_order(&[tag.wrapping_add(1); 32]),
    }
}

/// Returns the witnessed transaction ID of `tx`.
fn wtx_id_of(tx: &Transaction) -> WtxId {
    WtxId {
        id: tx.hash(),
        auth_digest: tx
            .auth_digest()
            .expect("a v5 transaction has an authorizing-data digest"),
    }
}

/// Returns a cacheable verification item.
fn cacheable_item(
    bundle: &Bundle<Authorized, ZatBalance>,
    sighash: SigHash,
    wtx_id: WtxId,
) -> Item {
    Item::new_with_wtx_id(bundle.clone(), sighash, wtx_id)
}

/// Returns the cache key for `bundle`'s pool, `sighash`, and `wtx_id`.
fn cache_key(bundle: &Bundle<Authorized, ZatBalance>, sighash: SigHash, wtx_id: WtxId) -> CacheKey {
    cacheable_item(bundle, sighash, wtx_id)
        .cache_key()
        .expect("an item constructed with a wtxid is cacheable")
}

#[test]
fn cache_key_is_deterministic() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let wtx_id = test_wtx_id(1);

    assert_eq!(
        cache_key(&bundle, sighash, wtx_id),
        cache_key(&bundle, sighash, wtx_id),
        "the same wtxid, sighash, and pool must always produce the same key"
    );
}

#[test]
fn cache_key_commits_to_the_txid_and_authorizing_data() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let original_wtx_id = test_wtx_id(1);
    let original = cache_key(&bundle, sighash, original_wtx_id);

    let mut different_txid = original_wtx_id;
    different_txid.id.0[0] ^= 1;
    assert_ne!(
        original,
        cache_key(&bundle, sighash, different_txid),
        "the transaction ID must be part of the cache key"
    );

    let mut different_authorizing_data = original_wtx_id;
    different_authorizing_data.auth_digest.0[0] ^= 1;
    assert_ne!(
        original,
        cache_key(&bundle, sighash, different_authorizing_data),
        "the authorizing-data digest must be part of the cache key"
    );
}

#[test]
fn cache_key_commits_to_the_sighash() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let wtx_id = test_wtx_id(1);
    let original = cache_key(&bundle, sighash, wtx_id);

    let mut different_sighash = sighash;
    different_sighash.0[0] ^= 1;

    assert_ne!(
        original,
        cache_key(&bundle, different_sighash, wtx_id),
        "different verification contexts must get different cache keys"
    );
}

/// Corrupting only the authorizing data changes the cache key, even though the txid does not
/// change.
///
/// That is the shape of CVE-2026-34377: a key derived from the txid alone would collide here, and
/// the corrupt transaction would inherit the valid one's verification. The witnessed ID's ZIP 244
/// authorizing-data digest is what keeps the two apart.
#[test]
fn cache_key_changes_when_the_authorizing_data_changes() {
    let (tx, bundle, sighash) = pre_nu6_2_transaction_bundle_and_sighash();
    let original = cache_key(&bundle, sighash, wtx_id_of(&tx));

    let garbage = with_garbage_orchard_authorization(tx.clone());
    assert_eq!(
        tx.hash(),
        garbage.hash(),
        "corrupting authorizing data must leave the txid unchanged, or this test proves nothing"
    );

    let garbage_sighasher = garbage
        .sighasher(NetworkUpgrade::Nu5, Arc::new(Vec::new()))
        .expect("the corrupt transaction still has a sighasher");
    let garbage_bundle = garbage_sighasher
        .orchard_bundle()
        .expect("the corrupt transaction still has an Orchard bundle");
    let garbage_sighash = garbage_sighasher.sighash(HashType::ALL, None);

    assert_ne!(
        original,
        cache_key(&garbage_bundle, garbage_sighash, wtx_id_of(&garbage)),
        "corrupting the proof and signatures must change the cache key"
    );
}

/// Returns `bundle`'s parts rebuilt under `flags` and `version`.
fn rebuilt_as(
    bundle: &Bundle<Authorized, ZatBalance>,
    flags: Flags,
    version: BundleVersion,
) -> Bundle<Authorized, ZatBalance> {
    Bundle::try_from_parts(
        bundle.actions().clone(),
        flags,
        *bundle.value_balance(),
        *bundle.anchor(),
        bundle.authorization().clone(),
        version,
    )
    .expect("a real mainnet Orchard bundle's parts are representable under the given version")
}

/// The Orchard and Ironwood pools of one v6 transaction never share a cache key.
///
/// A v6 transaction gives both bundles the same [`WtxId`]. Both pools also use the NU6.3 circuit
/// and therefore the same cache, so the value-pool tag names which bundle slot earned an entry.
///
/// The two bundles here are built from identical parts, and with cross-address transfers disabled
/// their flags are identical too. Only the pool tag tells them apart.
#[test]
fn cache_key_distinguishes_the_orchard_and_ironwood_pools() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let wtx_id = test_wtx_id(1);

    // Cross-address transfers are disallowed in the NU6.3 Orchard pool and optional in Ironwood,
    // so this is the one flag set both pools can encode.
    let orchard = rebuilt_as(
        &bundle,
        Flags::CROSS_ADDRESS_DISABLED,
        BundleVersion::orchard_v3(),
    );
    let ironwood = rebuilt_as(
        &bundle,
        Flags::CROSS_ADDRESS_DISABLED,
        BundleVersion::ironwood_v3(),
    );

    assert_eq!(
        orchard.flags(),
        ironwood.flags(),
        "this test is only meaningful if the two pools carry the same flags"
    );
    assert_ne!(
        cache_key(&orchard, sighash, wtx_id),
        cache_key(&ironwood, sighash, wtx_id),
        "the Orchard and Ironwood bundles of one transaction must not share a cache key"
    );
}

// Caching behaviour.

/// An inner verification service that counts calls and returns a fixed result.
#[derive(Clone)]
struct CountingVerifier {
    calls: Arc<AtomicUsize>,
    succeeds: bool,
}

impl CountingVerifier {
    fn new(succeeds: bool) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            succeeds,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Service<Item> for CountingVerifier {
    type Response = ();
    type Error = BoxError;
    type Future = future::Ready<Result<(), BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _item: Item) -> Self::Future {
        self.calls.fetch_add(1, Ordering::SeqCst);

        future::ready(if self.succeeds {
            Ok(())
        } else {
            Err(TransactionError::Halo2VerificationFailed.into())
        })
    }
}

#[tokio::test]
async fn cache_skips_the_inner_service_for_an_already_verified_item() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for _ in 0..3 {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(cacheable_item(&bundle, sighash, test_wtx_id(1)))
            .await
            .expect("a valid item must verify");
    }

    assert_eq!(
        inner.calls(),
        1,
        "only the first verification of an item may reach the inner service"
    );
}

#[tokio::test]
async fn items_without_a_wtxid_are_not_cached() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for _ in 0..2 {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(Item::new(bundle.clone(), sighash))
            .await
            .expect("an item without a wtxid must still verify");
    }

    assert_eq!(
        inner.calls(),
        2,
        "an item without a wtxid must never inherit a cached result"
    );
}

#[tokio::test]
async fn cache_does_not_reuse_a_result_across_items() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();

    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for wtx_id in [test_wtx_id(1), test_wtx_id(2)] {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(cacheable_item(&bundle, sighash, wtx_id))
            .await
            .expect("the inner service accepts everything in this test");
    }

    assert_eq!(
        inner.calls(),
        2,
        "items with different keys must each be verified"
    );
}

/// A failure is never remembered.
///
/// A batch error is not per-item evidence — `Fallback` resolves those by re-verifying singly —
/// and an error can report that the batch worker shut down rather than that a proof is invalid.
/// Remembering either as "invalid" would make the node reject valid blocks.
#[tokio::test]
async fn cache_does_not_remember_failures() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let inner = CountingVerifier::new(false);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for _ in 0..3 {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(cacheable_item(&bundle, sighash, test_wtx_id(1)))
            .await
            .expect_err("the inner service rejects everything in this test");
    }

    assert_eq!(
        inner.calls(),
        3,
        "a failed verification must be retried, not remembered"
    );
}

/// Verifies `item` through `verifier`, asserting that it succeeds.
async fn verify_through<S>(verifier: &mut Cached<S>, item: Item)
where
    S: Service<Item, Response = (), Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    verifier
        .ready()
        .await
        .expect("the cache must become ready")
        .call(item)
        .await
        .expect("the inner service accepts everything in this test");
}

#[tokio::test]
async fn cache_evicts_in_insertion_order_and_stays_correct_when_full() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 2, TEST_CACHE_VERIFIER_LABEL);

    let wtx_ids = [test_wtx_id(1), test_wtx_id(2), test_wtx_id(3)];

    for wtx_id in &wtx_ids {
        verify_through(&mut verifier, cacheable_item(&bundle, sighash, *wtx_id)).await;
    }
    assert_eq!(
        inner.calls(),
        3,
        "three distinct items, three verifications"
    );

    // The two most recent are still remembered.
    for wtx_id in &wtx_ids[1..] {
        verify_through(&mut verifier, cacheable_item(&bundle, sighash, *wtx_id)).await;
    }
    assert_eq!(inner.calls(), 3, "entries within the capacity must be kept");

    // The oldest was evicted, so it is verified again rather than silently mis-answered.
    verify_through(&mut verifier, cacheable_item(&bundle, sighash, wtx_ids[0])).await;
    assert_eq!(inner.calls(), 4, "an evicted entry must be re-verified");
}

/// A remembered result is never visible to another circuit version's cache.
///
/// The cache key deliberately does not name the verifying key. What binds an entry to the key it
/// was produced under is which cache holds it — [`batch_verifier`](super::batch_verifier) builds
/// one per circuit version, and [`orchard_verifier_routing_selects_the_correct_key`] pins the
/// routing. This pins the other half: two cache instances share no state, so an item verified
/// under the pre-NU6.2 insecure key can never be answered from that entry when it is later routed
/// to a different era's verifier.
#[tokio::test]
async fn a_result_cached_under_one_era_is_not_visible_to_another() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let item = cacheable_item(&bundle, sighash, test_wtx_id(9));

    let inner = CountingVerifier::new(true);
    let mut one_era = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);
    let mut another_era = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    verify_through(&mut one_era, item.clone()).await;
    assert_eq!(inner.calls(), 1, "the first verification must be a miss");

    verify_through(&mut another_era, item).await;
    assert_eq!(
        inner.calls(),
        2,
        "another era's cache must not answer from an entry this one never recorded"
    );
}

/// Clones of a cache answer from the same set of verified proofs.
///
/// Production never calls a global verifier directly: the routing functions hand out a `&'static`
/// handle and every request goes through a fresh `.clone()` of it. A cache that lived in the
/// handle rather than behind the shared `Arc` would be empty for every request, so this pins the
/// sharing that makes the cache reachable at all.
#[tokio::test]
async fn cache_is_shared_between_clones() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let item = cacheable_item(&bundle, sighash, test_wtx_id(1));

    let inner = CountingVerifier::new(true);
    let verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    let mut warming_clone = verifier.clone();
    verify_through(&mut warming_clone, item.clone()).await;
    assert_eq!(inner.calls(), 1, "the first verification must be a miss");

    let mut reading_clone = verifier.clone();
    verify_through(&mut reading_clone, item).await;
    assert_eq!(
        inner.calls(),
        1,
        "a clone must answer from the result another clone recorded"
    );
}

/// An inner service that never returns a result, standing in for a verification in flight.
#[derive(Clone)]
struct PendingVerifier {
    calls: Arc<AtomicUsize>,
}

impl PendingVerifier {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Service<Item> for PendingVerifier {
    type Response = ();
    type Error = BoxError;
    type Future = future::Pending<Result<(), BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _item: Item) -> Self::Future {
        self.calls.fetch_add(1, Ordering::SeqCst);
        future::pending()
    }
}

/// A verification that is cancelled before it returns is not remembered.
///
/// The cache records a key from inside the response future, so dropping that future has to leave
/// the cache untouched. Recording on the way in would remember a proof that was never checked:
/// callers drop these futures routinely, because a block or mempool verification abandons its
/// remaining checks as soon as one of them fails.
#[tokio::test]
async fn cancelling_a_verification_does_not_populate_the_cache() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let item = cacheable_item(&bundle, sighash, test_wtx_id(1));

    let hanging = PendingVerifier::new();
    let mut verifier = Cached::new(hanging.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    // Start a verification and drop it before the inner service can answer.
    let in_flight = verifier
        .ready()
        .await
        .expect("the cache must become ready")
        .call(item.clone());
    tokio::time::timeout(Duration::from_millis(50), in_flight)
        .await
        .expect_err("the inner service never returns, so the verification cannot complete");
    assert_eq!(
        hanging.calls(),
        1,
        "the cancelled verification must have reached the inner service"
    );

    // Retrying must verify again rather than read an entry the cancelled call never earned.
    let mut verifier = verifier.with_inner(CountingVerifier::new(true));
    let counting = verifier.inner().clone();
    verify_through(&mut verifier, item).await;
    assert_eq!(
        counting.calls(),
        1,
        "a cancelled verification must not be remembered as a success"
    );
}

/// An inner service whose readiness always fails, standing in for a dead batch worker.
///
/// `Batch::poll_ready` reports an error when its worker has exited, panicked, or closed its
/// channel. `call` panics here because it must never be reached: a service that is not ready must
/// not be called, and the tests below are about what happens *before* that point.
#[derive(Clone)]
struct UnreadyVerifier {
    poll_readies: Arc<AtomicUsize>,
}

impl UnreadyVerifier {
    fn new() -> Self {
        Self {
            poll_readies: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn poll_readies(&self) -> usize {
        self.poll_readies.load(Ordering::SeqCst)
    }
}

impl Service<Item> for UnreadyVerifier {
    type Response = ();
    type Error = BoxError;
    type Future = future::Ready<Result<(), BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_readies.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Err(BoxError::from("batch worker finished unexpectedly")))
    }

    fn call(&mut self, _item: Item) -> Self::Future {
        unreachable!("a service whose poll_ready failed must not be called")
    }
}

/// A cache hit is answered even when the inner service can no longer become ready.
///
/// `Cached::poll_ready` must not delegate to the inner service. Callers poll readiness before
/// `call`, so delegating would surface a dead batch worker's error for an item whose result the
/// cache already holds — reporting a verified proof as a verification failure, and rejecting a
/// valid block. That is the "an error need not be a verdict" case the module docs are about.
#[tokio::test]
async fn cache_hit_survives_an_inner_service_that_never_becomes_ready() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let item = cacheable_item(&bundle, sighash, test_wtx_id(1));

    // Warm the cache through a healthy inner service.
    let healthy = CountingVerifier::new(true);
    let mut verifier = Cached::new(healthy.clone(), 8, TEST_CACHE_VERIFIER_LABEL);
    verify_through(&mut verifier, item.clone()).await;
    assert_eq!(healthy.calls(), 1, "the first verification must be a miss");

    // Swap in an inner service that can never become ready, keeping the same cache.
    let dead = UnreadyVerifier::new();
    let mut verifier = verifier.with_inner(dead.clone());

    verifier
        .ready()
        .await
        .expect("the cache must be ready even when the inner service is not")
        .call(item)
        .await
        .expect("a cache hit must be answered from the cache, not from the dead inner service");

    assert_eq!(
        dead.poll_readies(),
        0,
        "a hit must not poll the inner service for readiness at all"
    );
}

/// A miss still propagates an inner readiness failure.
///
/// Moving readiness off `poll_ready` must not make the cache swallow it: an item that is not in
/// the cache has to reach the inner service, and if that service cannot become ready the request
/// must fail rather than be reported as verified.
#[tokio::test]
async fn cache_miss_propagates_an_inner_readiness_failure() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();
    let dead = UnreadyVerifier::new();
    let mut verifier = Cached::new(dead.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    verifier
        .ready()
        .await
        .expect("the cache itself is always ready")
        .call(cacheable_item(&bundle, sighash, test_wtx_id(1)))
        .await
        .expect_err("a miss must surface the inner service's readiness failure");

    assert!(
        dead.poll_readies() > 0,
        "a miss must acquire inner readiness"
    );

    // And the failure must not be remembered as a success.
    let mut verifier = verifier.with_inner(CountingVerifier::new(true));
    let counting = verifier.inner().clone();
    verify_through(
        &mut verifier,
        cacheable_item(&bundle, sighash, test_wtx_id(1)),
    )
    .await;
    assert_eq!(
        counting.calls(),
        1,
        "the item must still be verified, so the readiness failure was not recorded as an Ok"
    );
}
