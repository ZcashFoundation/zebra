//! Exact-body auth digest memoization and invalidation.

use std::sync::{Arc, Barrier};

use orchard::{bundle::Authorized, Proof, ValuePool};
use zcash_protocol::consensus::BranchId;

use crate::{
    block::{Block, Height},
    parameters::NetworkUpgrade,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{
        arbitrary::shielded::{fake_bundle_for_branch, fake_v6_transaction},
        AuthDigest, Hash, Transaction,
    },
    transparent,
};

fn uncached_digest(tx: &Transaction) -> Option<AuthDigest> {
    (tx.version() >= 5)
        .then(|| AuthDigest(tx.inner().auth_commitment().as_ref().try_into().unwrap()))
}

fn check_cache(tx: &Transaction) {
    assert!(tx.1.get().is_none(), "new bodies must start unhashed");
    let debug = format!("{tx:?}");
    let pretty_debug = format!("{tx:#?}");
    let bytes = tx.zcash_serialize_to_vec().unwrap();
    let txid = tx.hash();
    let expected = uncached_digest(tx);
    for _ in 0..2 {
        assert_eq!(tx.auth_digest(), expected);
        assert_eq!(tx.1.get().copied(), expected);
    }
    let clone = tx.clone();
    assert_eq!(clone.1.get().copied(), expected);
    assert_eq!(clone.auth_digest(), expected);
    assert_eq!(clone.hash(), txid);
    assert_eq!(clone, *tx);
    assert_eq!(clone.unmined_id(), tx.unmined_id());
    assert_eq!(clone.zcash_serialize_to_vec().unwrap(), bytes);
    assert_eq!(format!("{tx:?}"), debug);
    assert_eq!(format!("{tx:#?}"), pretty_debug);
    assert_eq!(debug, format!("Transaction({:?})", tx.inner()));
}

#[test]
fn auth_digest_cache_matches_native_all_versions() {
    let _init_guard = zebra_test::init();
    let mut versions = std::collections::BTreeSet::new();
    for bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(*bytes).unwrap();
        for tx in block.transactions {
            versions.insert(tx.version());
            check_cache(&tx);
        }
    }
    let zip244: Vec<_> = zebra_test::zip0244::TEST_VECTORS
        .iter()
        .map(|vector| {
            let tx = Transaction::zcash_deserialize(vector.tx.as_slice()).unwrap();
            check_cache(&tx);
            assert_eq!(tx.auth_digest().unwrap().0, vector.auth_digest);
            tx
        })
        .collect();
    let sapling = zip244
        .iter()
        .find_map(|tx| tx.sapling_bundle().cloned())
        .unwrap();
    for pools in 0..8 {
        let tx = Transaction::from_inner(
            zcash_primitives::transaction::TransactionData::from_parts_v6(
                BranchId::Nu6_3,
                0,
                0.into(),
                None,
                (pools & 1 != 0).then(|| sapling.clone()),
                (pools & 2 != 0).then(|| {
                    fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Orchard, 2, 1).unwrap()
                }),
                (pools & 4 != 0).then(|| {
                    fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Ironwood, 2, 2).unwrap()
                }),
            )
            .freeze()
            .unwrap(),
        );
        versions.insert(tx.version());
        check_cache(&tx);
    }
    assert_eq!(versions, (1..=6).collect());

    // Only a digest and cell state are retained, rounded up to the wrapper's alignment.
    let overhead = std::mem::size_of::<Transaction>()
        - std::mem::size_of::<zcash_primitives::transaction::Transaction>();
    assert!(
        overhead
            <= std::mem::size_of::<std::sync::OnceLock<AuthDigest>>()
                + std::mem::align_of::<Transaction>()
    );
}

#[test]
fn auth_digest_cache_concurrent_first_readers() {
    let _init_guard = zebra_test::init();
    let tx = Arc::new(fake_v6_transaction(
        NetworkUpgrade::Nu6_3,
        fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Orchard, 2, 1),
        fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Ironwood, 2, 2),
    ));
    let expected = uncached_digest(&tx);
    assert!(tx.1.get().is_none());
    let barrier = Barrier::new(8);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let tx = Arc::clone(&tx);
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                assert_eq!(tx.auth_digest(), expected);
                assert_eq!(tx.1.get().copied(), expected);
            });
        }
    });
    assert_eq!(tx.1.get().copied(), expected);
}

#[test]
fn auth_digest_cache_rebuilt_bodies_are_independent() {
    let _init_guard = zebra_test::init();
    for version in [5, 6] {
        let branch = if version == 5 {
            BranchId::Nu5
        } else {
            BranchId::Nu6_3
        };
        let orchard = fake_bundle_for_branch(branch, ValuePool::Orchard, 2, 1);
        let tx = if version == 5 {
            Transaction::test_v5_with_orchard(
                NetworkUpgrade::Nu5,
                vec![],
                vec![],
                crate::transaction::LockTime::unlocked(),
                Height(1),
                orchard,
            )
        } else {
            fake_v6_transaction(NetworkUpgrade::Nu6_3, orchard, None)
        }
        .with_transparent_inputs(vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: Hash([1; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[0x51]),
            sequence: 0,
        }]);
        check_cache(&tx);
        let original_digest = tx.auth_digest();

        let mut inputs = tx.inputs();
        let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
            unreachable!()
        };
        *unlock_script = transparent::Script::new(&[0x52]);
        let changed_script = tx.clone().with_transparent_inputs(inputs);
        check_cache(&changed_script);
        assert_eq!(changed_script.hash(), tx.hash());
        assert_ne!(changed_script.auth_digest(), original_digest);

        // Change proofs, binding signatures, and spend signatures without changing the txid.
        for change in 0..3 {
            let bundle = tx.orchard_bundle().unwrap().clone().map_authorization(
                &mut (),
                |_, _, signature| {
                    if change == 2 {
                        [1; 64].into()
                    } else {
                        signature
                    }
                },
                |_, auth| {
                    let mut proof = auth.proof().as_ref().to_vec();
                    if change == 0 {
                        proof[0] ^= 1;
                    }
                    let signature = if change == 1 {
                        [1; 64].into()
                    } else {
                        auth.binding_signature().clone()
                    };
                    Authorized::from_parts(Proof::new(proof), signature)
                },
            );
            let rebuilt = tx.clone().with_orchard_bundle(Some(bundle));
            check_cache(&rebuilt);
            assert_eq!(rebuilt, tx, "txid equality must not imply cache identity");
            assert_ne!(rebuilt.auth_digest(), original_digest);
            assert_ne!(rebuilt.unmined_id(), tx.unmined_id());
            let decoded = Transaction::zcash_deserialize(
                rebuilt.zcash_serialize_to_vec().unwrap().as_slice(),
            )
            .unwrap();
            check_cache(&decoded);
            assert_eq!(decoded.auth_digest(), rebuilt.auth_digest());
        }

        let output =
            transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&[0x51]));
        let mut rebuilt = tx.clone().with_transparent_outputs(vec![output.clone()]);
        check_cache(&rebuilt);
        rebuilt.set_expiry_height(Height(2));
        check_cache(&rebuilt);
        rebuilt.set_network_upgrade(tx.network_upgrade().unwrap());
        check_cache(&rebuilt);
        rebuilt.set_outputs(vec![output]);
        check_cache(&rebuilt);
        rebuilt.for_each_value_mut(|value| *value = 2.try_into().unwrap());
        check_cache(&rebuilt);
        assert_eq!(tx.auth_digest(), original_digest);
        assert_eq!(tx.1.get().copied(), original_digest);
    }
}
