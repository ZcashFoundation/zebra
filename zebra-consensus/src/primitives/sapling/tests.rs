//! Tests for the Sapling bundle verifier.
//!
//! Most of these are cache-key completeness tests. [`Cached`] reuses a previous `Ok` for any item
//! whose key matches, so a key that misses one of verification's inputs is a consensus bug: it
//! would accept a bundle that was never checked.
//!
//! Sapling keys its entries the same way Halo2 does — transaction ID, sighash and pool — but its
//! bundles also appear in v4 transactions, which have no witnessed ID. A v4 transaction's legacy
//! ID is the hash of its whole serialization, so it covers the proofs and signatures directly,
//! and its sighash is what carries the block's consensus branch id into the key.

use std::{
    future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use tower::{Service, ServiceExt};
use tower_batch_control::RequestWeight;
use zebra_chain::{
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::{HashType, Transaction, TxVersion},
    transparent,
};

use crate::{error::TransactionError, BoxError};

use super::{batch_fallback_verifier, BatchFallbackService, CacheKey, Cached, CachedItem, Item};

/// The `verifier` label the test caches report their metrics under.
///
/// Test caches use their own label so their counts never land in the series the production
/// verifier reports.
const TEST_CACHE_VERIFIER_LABEL: &str = "groth16_sapling_test";

/// Returns the mainnet test transactions that carry a Sapling bundle, with the network upgrade
/// each one was mined under.
///
/// Transactions with transparent inputs are skipped, because their sighash needs the previous
/// outputs they spend, which are not in the test vectors.
///
/// The upgrade matters for the tests that actually verify: a V4 sighash commits to the consensus
/// branch id, so a real bundle only verifies under the upgrade its block was mined in. The
/// cache-key tests do not need a valid sighash and pass an upgrade of their own.
fn mined_sapling_transactions() -> Vec<(NetworkUpgrade, Transaction)> {
    let mut transactions = Vec::new();

    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("hard-coded test vector must deserialize");

        let nu = NetworkUpgrade::current(&Network::Mainnet, Height(*height));

        for tx in &block.transactions {
            if !tx.inputs().is_empty() {
                continue;
            }

            if item(tx, nu).is_some() {
                transactions.push((nu, tx.as_ref().clone()));
            }
        }
    }

    assert!(
        !transactions.is_empty(),
        "mainnet test blocks must contain a transparent-input-free Sapling transaction"
    );

    transactions
}

/// Returns the mainnet test transactions that carry a Sapling bundle.
fn sapling_transactions() -> Vec<Transaction> {
    mined_sapling_transactions()
        .into_iter()
        .map(|(_, tx)| tx)
        .collect()
}

/// Returns one real mainnet Sapling transaction.
fn sapling_transaction() -> Transaction {
    sapling_transactions()
        .into_iter()
        .next()
        .expect("there is at least one Sapling transaction")
}

/// Returns one real mainnet V4 Sapling transaction, with the network upgrade it was mined under.
fn mined_v4_sapling_transaction() -> (NetworkUpgrade, Transaction) {
    mined_sapling_transactions()
        .into_iter()
        .find(|(_, tx)| tx.tx_version() == TxVersion::V4)
        .expect("mainnet test blocks must contain a V4 Sapling transaction")
}

/// Returns the verification item for `tx`'s Sapling bundle under `nu`, if it has one.
fn item(tx: &Transaction, nu: NetworkUpgrade) -> Option<Item> {
    let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());
    let sighasher = tx.sighasher(nu, all_previous_outputs).ok()?;
    let bundle = sighasher.sapling_bundle()?;

    Some(Item::new(
        bundle,
        sighasher.sighash(HashType::ALL, None),
        tx.unmined_id(),
    ))
}

/// Returns the cache key of `tx`'s Sapling bundle under `nu`.
fn cache_key(tx: &Transaction, nu: NetworkUpgrade) -> CacheKey {
    item(tx, nu)
        .expect("the transaction was selected for having a Sapling bundle")
        .cache_key()
        .expect("every Sapling item carries a cache key")
}

#[test]
fn request_weight_counts_spend_and_output_proofs() {
    let mut has_multiple_proofs = false;

    for (nu, tx) in mined_sapling_transactions() {
        let proof_count = tx.sapling_spends().count() + tx.sapling_outputs().count();
        let item = item(&tx, nu).expect("the transaction was selected for its Sapling bundle");

        assert_eq!(item.request_weight(), proof_count);
        has_multiple_proofs |= proof_count > 1;
    }

    assert!(
        has_multiple_proofs,
        "test vectors must distinguish proof count from the default weight of one"
    );
}

#[test]
fn cache_key_is_deterministic() {
    let tx = sapling_transaction();

    assert_eq!(
        cache_key(&tx, NetworkUpgrade::Nu5),
        cache_key(&tx, NetworkUpgrade::Nu5),
        "the same transaction, sighash, and pool must always produce the same key"
    );
}

#[test]
fn cache_key_distinguishes_different_transactions() {
    let transactions = sapling_transactions();
    let first = transactions.first().expect("there is a first transaction");
    let second = transactions
        .iter()
        .find(|tx| tx.hash() != first.hash())
        .expect("mainnet test blocks must contain two distinct Sapling transactions");

    assert_ne!(
        cache_key(first, NetworkUpgrade::Nu5),
        cache_key(second, NetworkUpgrade::Nu5),
        "two transactions must not share a cache key"
    );
}

/// The sighash separates two verifications of one transaction's bundle.
///
/// One transaction has one transaction ID whatever height it is verified at, but a V4 sighash
/// commits to the consensus branch id of the block that mines it (ZIP 143 and ZIP 243 put it in
/// the BLAKE2b personalization). The sighash is what carries that into the key.
#[test]
fn cache_key_commits_to_the_sighash() {
    let (_nu, tx) = mined_v4_sapling_transaction();

    assert_ne!(
        cache_key(&tx, NetworkUpgrade::Canopy),
        cache_key(&tx, NetworkUpgrade::Nu5),
        "the same bundle verified under two branch ids must not share a cache key"
    );
}

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
            Err(TransactionError::SaplingVerificationFailed.into())
        })
    }
}

#[tokio::test]
async fn cache_skips_the_inner_service_for_an_already_verified_bundle() {
    let tx = sapling_transaction();
    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for _ in 0..3 {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(item(&tx, NetworkUpgrade::Nu5).expect("the transaction has a bundle"))
            .await
            .expect("a valid item must verify");
    }

    assert_eq!(
        inner.calls(),
        1,
        "only the first verification of a bundle may reach the inner service"
    );
}

#[tokio::test]
async fn cache_does_not_reuse_a_result_across_bundles() {
    let transactions = sapling_transactions();
    let inner = CountingVerifier::new(true);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for tx in transactions.iter().take(2) {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(item(tx, NetworkUpgrade::Nu5).expect("the transaction has a bundle"))
            .await
            .expect("the inner service accepts everything in this test");
    }

    assert_eq!(
        inner.calls(),
        2,
        "bundles with different keys must each be verified"
    );
}

/// A failure is never remembered.
///
/// A batch error is not per-item evidence — `Fallback` resolves those by re-verifying singly —
/// and an error can report that the batch worker shut down rather than that a proof is invalid.
/// Remembering either as "invalid" would make the node reject valid blocks.
#[tokio::test]
async fn cache_does_not_remember_failures() {
    let tx = sapling_transaction();
    let inner = CountingVerifier::new(false);
    let mut verifier = Cached::new(inner.clone(), 8, TEST_CACHE_VERIFIER_LABEL);

    for _ in 0..3 {
        verifier
            .ready()
            .await
            .expect("the cache must become ready")
            .call(item(&tx, NetworkUpgrade::Nu5).expect("the transaction has a bundle"))
            .await
            .expect_err("the inner service rejects everything in this test");
    }

    assert_eq!(
        inner.calls(),
        3,
        "a failed verification must not be remembered"
    );
}

/// The real verification stack, behind a cache private to one test.
///
/// The production [`VERIFIER`](super::VERIFIER) is a process-wide `Lazy`, so its cache carries
/// whatever every other test in this binary has already verified. This builds the same batch and
/// fallback stack behind a fresh cache, so a test can prove what a cold cache does with real
/// Sapling verification underneath.
fn uncached_verification_behind_a_fresh_cache() -> Cached<BatchFallbackService> {
    Cached::new(batch_fallback_verifier(), 8, TEST_CACHE_VERIFIER_LABEL)
}

/// A bundle verified under one network upgrade is not reused under another.
///
/// The sighash is the only key component that separates these two verifications, so this is the
/// end-to-end evidence for keying on it at all: the same bundle, the same transaction ID, a
/// different branch id, and the remembered `Ok` must not answer it.
///
/// Real verification runs underneath, so the second call is not merely a miss — the mainnet
/// signatures do not verify against another era's sighash, and the rejection proves the bundle
/// was actually checked rather than answered from the cache.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_verified_under_one_upgrade_is_not_reused_under_another() {
    let _init_guard = zebra_test::init();

    let (mined_upgrade, tx) = mined_v4_sapling_transaction();

    // Any other upgrade that accepts V4 transactions will do: only its branch id matters here.
    let other_upgrade = if mined_upgrade == NetworkUpgrade::Nu5 {
        NetworkUpgrade::Canopy
    } else {
        NetworkUpgrade::Nu5
    };

    let mined_item = item(&tx, mined_upgrade).expect("the transaction has a bundle");
    let other_item = item(&tx, other_upgrade).expect("the transaction has a bundle");
    assert_eq!(
        (
            mined_item.bundle.shielded_spends().len(),
            mined_item.bundle.shielded_outputs().len(),
            *mined_item.bundle.value_balance(),
        ),
        (
            other_item.bundle.shielded_spends().len(),
            other_item.bundle.shielded_outputs().len(),
            *other_item.bundle.value_balance(),
        ),
        "the branch id must not change the parsed bundle, or this test proves nothing"
    );
    assert_ne!(
        mined_item.cache_key(),
        other_item.cache_key(),
        "the two sighashes must produce different keys"
    );

    let verifier = uncached_verification_behind_a_fresh_cache();

    verifier
        .clone()
        .oneshot(mined_item)
        .await
        .expect("a real mainnet Sapling bundle must verify under the upgrade that mined it");

    let error = verifier
        .clone()
        .oneshot(other_item)
        .await
        .expect_err("the same bundle must not verify against another upgrade's sighash");

    let error = error
        .downcast::<TransactionError>()
        .expect("the verifier reports a typed transaction error");
    assert!(
        matches!(*error, TransactionError::SaplingVerificationFailed),
        "expected SaplingVerificationFailed, got: {error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn block_flush_resolves_every_sapling_shard_without_timers() {
    use crate::primitives::flush::{tests, BlockFlush, Queueing, Verifier};

    let _init_guard = zebra_test::init();
    let (nu, tx) = mined_v4_sapling_transaction();
    let item = item(&tx, nu).unwrap();
    assert!(item.request_weight() * 2 < crate::primitives::MAX_BATCH_SIZE);
    let shards = super::ShardedVerifier::new(2);
    let flush_shards = shards.clone();
    let admissions = shards.next.clone();
    let mut cache = Cached::new(
        tower_fallback::Fallback::new(shards, tower::service_fn(super::verify_single)),
        8,
        TEST_CACHE_VERIFIER_LABEL,
    )
    .with_flush(Verifier::Sapling);
    let context = Arc::new(());
    let _registration = BlockFlush::register(&context, 1);
    let flushes = Arc::new(AtomicUsize::new(0));
    let count = flushes.clone();
    let start = tokio::time::Instant::now();
    tests::FLUSH
        .scope(
            Box::new(move |verifier| {
                assert!(matches!(verifier, Verifier::Sapling));
                count.fetch_add(1, Ordering::SeqCst);
                flush_shards.try_flush()
            }),
            async {
                let transaction = Queueing::for_transaction(&context).unwrap();
                let responses = transaction
                    .scope(|| (0..4).map(|_| cache.call(item.clone())).collect::<Vec<_>>());
                drop(transaction);
                for result in
                    tests::without_advancing_time(futures::future::join_all(responses)).await
                {
                    result.expect("bundles in every shard must verify before any timer expires");
                }
            },
        )
        .await;
    assert_eq!(admissions.load(Ordering::Relaxed), 4);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(tokio::time::Instant::now(), start);
}

#[tokio::test]
async fn malformed_bundle_does_not_send_valid_bundles_to_fallback() {
    let _init_guard = zebra_test::init();
    let (nu, tx) = mined_v4_sapling_transaction();
    let valid = item(&tx, nu).unwrap();
    let mut malformed = valid.clone();
    malformed.bundle = malformed.bundle.map_authorization(
        (),
        |_, _| [0; 192],
        |_, _| [0; 192],
        |_, sig| sig,
        |_, auth| auth,
    );
    assert!(valid.request_weight() * 3 < crate::primitives::MAX_BATCH_SIZE);
    let fallbacks = Arc::new(AtomicUsize::new(0));
    let count = fallbacks.clone();
    let mut verifier = tower_fallback::Fallback::new(
        super::ShardedVerifier::new(1),
        tower::service_fn(move |item| {
            count.fetch_add(1, Ordering::SeqCst);
            super::verify_single(item)
        }),
    );
    let mut responses = Vec::new();
    for item in [valid.clone(), malformed, valid] {
        responses.push(verifier.ready().await.unwrap().call(item));
    }
    assert!(verifier.primary().try_flush().unwrap());
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        futures::future::join_all(responses),
    )
    .await
    .expect("verification must finish");
    assert!(results[0].is_ok());
    assert!(results[1].is_err());
    assert!(results[2].is_ok());
    assert_eq!(fallbacks.load(Ordering::SeqCst), 1);
}

#[test]
fn single_verifications_share_the_rayon_cpu_budget() {
    let _init_guard = zebra_test::init();
    let (nu, tx) = mined_v4_sapling_transaction();
    let item = item(&tx, nu).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();

    pool.install(|| {
        let mut responses = [
            super::verify_single(item.clone()),
            super::verify_single(item),
        ];
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());

        // The only worker queues both fallbacks; neither can execute until it yields.
        for response in &mut responses {
            assert!(response.as_mut().poll(&mut cx).is_pending());
        }
        while matches!(rayon::yield_now(), Some(rayon::Yield::Executed)) {}
        for response in &mut responses {
            assert!(matches!(
                response.as_mut().poll(&mut cx),
                Poll::Ready(Ok(()))
            ));
        }
    });
}

#[test]
fn shards_respect_configured_cpu_budget() {
    for (parallelism, expected_shards) in [(1, 1), (4, 1), (8, 2), (32, 8), (64, 8)] {
        let shards = super::shard_count(parallelism);
        assert_eq!(shards, expected_shards);
        let max_batches = (parallelism / shards).max(1);
        assert!(shards * max_batches <= parallelism);
    }
}
