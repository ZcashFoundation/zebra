//! Tests for the shared verification cache key and the bounded store behind it.
//!
//! The verifiers pin their own key construction over real bundles. These cover the parts that are
//! the same for all of them: which of the key's three components separate two entries, and the
//! store's capacity and eviction.

use zebra_chain::{
    serialization::BytesInDisplayOrder,
    transaction::{AuthDigest, Hash, UnminedTxId, WtxId},
};

use super::{CacheKey, ShieldedPool, VerifiedBundles};

/// Returns a transaction ID with no witness, as a v1-v4 transaction has.
fn legacy_tx_id(tag: u8) -> UnminedTxId {
    UnminedTxId::Legacy(Hash::from_bytes_in_display_order(&[tag; 32]))
}

/// Returns a witnessed transaction ID, as a v5 or v6 transaction has.
fn witnessed_tx_id(txid_tag: u8, auth_digest_tag: u8) -> UnminedTxId {
    UnminedTxId::Witnessed(WtxId {
        id: Hash::from_bytes_in_display_order(&[txid_tag; 32]),
        auth_digest: AuthDigest::from_bytes_in_display_order(&[auth_digest_tag; 32]),
    })
}

/// The pool separates the bundles a v6 transaction carries under one ID and one sighash.
///
/// The Orchard and Ironwood caches are one cache from NU6.3 onward, so this is what keeps their
/// entries apart there.
#[test]
fn keys_for_the_same_transaction_differ_by_pool() {
    let tx_id = witnessed_tx_id(1, 2);
    let sighash = [3; 32];

    let keys = [
        CacheKey::new(tx_id, sighash, ShieldedPool::Sapling),
        CacheKey::new(tx_id, sighash, ShieldedPool::Orchard),
        CacheKey::new(tx_id, sighash, ShieldedPool::Ironwood),
    ];

    let unique: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(
        unique.len(),
        keys.len(),
        "one transaction's three shielded bundles must not share a cache key"
    );
}

/// The authorizing-data digest separates two witnessed IDs that share a txid.
///
/// Under ZIP 244 a v5 transaction's txid excludes its proofs and signatures, so a key that named
/// only the txid would answer both of these with one verification. That is CVE-2026-34377.
#[test]
fn witnessed_keys_differ_by_authorizing_data() {
    let sighash = [3; 32];

    assert_ne!(
        CacheKey::new(witnessed_tx_id(1, 2), sighash, ShieldedPool::Orchard),
        CacheKey::new(witnessed_tx_id(1, 4), sighash, ShieldedPool::Orchard),
        "the same txid with different authorizing data must not share a cache key"
    );
}

/// The sighash separates two verifications of one transaction's bundle.
///
/// The sighash is not a function of the transaction alone: the amounts and scripts of the spent
/// transparent outputs enter it, and those come from the verification context.
#[test]
fn keys_differ_by_sighash() {
    let tx_id = witnessed_tx_id(1, 2);

    assert_ne!(
        CacheKey::new(tx_id, [3; 32], ShieldedPool::Sapling),
        CacheKey::new(tx_id, [4; 32], ShieldedPool::Sapling),
        "the sighash is an input to verification, so it must be an input to the key"
    );
}

/// A legacy ID and a witnessed ID never collide, whatever they contain.
#[test]
fn legacy_and_witnessed_keys_differ() {
    let sighash = [3; 32];

    assert_ne!(
        CacheKey::new(legacy_tx_id(1), sighash, ShieldedPool::Sapling),
        CacheKey::new(witnessed_tx_id(1, 1), sighash, ShieldedPool::Sapling),
        "a v4 transaction ID must not collide with a witnessed one"
    );
}

/// Every Orchard value pool has a distinct tag.
#[test]
fn orchard_value_pools_map_to_distinct_tags() {
    assert_eq!(
        ShieldedPool::from(orchard::ValuePool::Orchard),
        ShieldedPool::Orchard
    );
    assert_eq!(
        ShieldedPool::from(orchard::ValuePool::Ironwood),
        ShieldedPool::Ironwood
    );
}

/// The lookup set and the eviction queue always hold the same keys.
///
/// They are two representations of one fact. A path that updated one without the other would
/// either drop a key that `contains` still answers — remembering a bundle for the rest of the
/// process — or grow the queue past the capacity it was built with.
#[test]
fn the_lookup_set_and_the_eviction_queue_hold_the_same_keys() {
    let mut verified = VerifiedBundles::new(2);
    let keys = [
        CacheKey::new(legacy_tx_id(1), [0; 32], ShieldedPool::Sapling),
        CacheKey::new(legacy_tx_id(2), [0; 32], ShieldedPool::Sapling),
        CacheKey::new(legacy_tx_id(3), [0; 32], ShieldedPool::Sapling),
    ];

    for key in keys {
        verified.insert(key);
        assert_eq!(
            verified.keys.len(),
            verified.insertion_order.len(),
            "the lookup set and the eviction queue must hold the same keys"
        );
        assert!(verified.keys.len() <= 2, "the capacity bounds the cache");
    }
    assert!(
        !verified.contains(&keys[0]),
        "the oldest key must be evicted"
    );

    let repeated = verified.insert(keys[2]);
    assert!(!repeated.inserted, "a concurrent duplicate is not recorded");
    assert_eq!(repeated.evicted, 0, "a duplicate must not evict anything");
    assert_eq!(verified.keys.len(), verified.insertion_order.len());

    verified.clear();
    assert!(verified.keys.is_empty() && verified.insertion_order.is_empty());
}

/// A poisoned cache must neither accept a cached item nor reject an unchecked item.
#[tokio::test]
async fn poisoned_cache_bypasses_lookups_and_inserts() {
    use super::{Cached, CachedItem};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[derive(Clone)]
    struct Item(CacheKey);

    impl CachedItem for Item {
        fn cache_key(&self) -> Option<CacheKey> {
            Some(self.0)
        }
    }

    let valid = Item(CacheKey::new(
        legacy_tx_id(1),
        [0; 32],
        ShieldedPool::Sapling,
    ));
    let invalid = Item(CacheKey::new(
        legacy_tx_id(2),
        [0; 32],
        ShieldedPool::Sapling,
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let inner = tower::service_fn({
        let calls = calls.clone();
        let valid = valid.clone();
        move |item: Item| {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(if item.0 == valid.0 {
                Ok(())
            } else {
                Err(crate::BoxError::from("invalid bundle"))
            })
        }
    });
    let verifier = Cached::new(inner, 2, "poison_test");
    verifier.clone().oneshot(valid.clone()).await.unwrap();

    let verified = verifier.verified.clone();
    assert!(std::panic::catch_unwind(move || {
        let mut verified = verified.lock().unwrap();
        // Model a panic between updates to the set and eviction queue.
        verified.insertion_order.clear();
        panic!("poison the cache");
    })
    .is_err());
    assert!(verifier.verified.is_poisoned());

    for _ in 0..2 {
        verifier.clone().oneshot(valid.clone()).await.unwrap();
    }
    assert!(verifier.clone().oneshot(invalid).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}
