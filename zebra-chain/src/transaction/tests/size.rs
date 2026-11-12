//! Decoded byte counts remain exact across cloning and transaction rebuilding.

use std::sync::Arc;

use crate::{
    block::{self, Block},
    parameters::NetworkUpgrade,
    serialization::{FakeWriter, ZcashDeserialize, ZcashSerialize},
    transaction::{
        arbitrary::shielded::{fake_bundle_for_branch, fake_v6_transaction},
        Transaction, TxVersion,
    },
    transparent,
};

fn assert_size(transaction: &Transaction) {
    let mut independent = FakeWriter(0);
    transaction.zcash_serialize(&mut independent).unwrap();
    assert_eq!(transaction.zcash_serialized_size(), independent.0);
    assert_eq!(
        Arc::new(transaction.clone()).zcash_serialized_size(),
        independent.0
    );
}

fn transactions() -> Vec<Arc<Transaction>> {
    let mut transactions = Vec::new();
    for bytes in zebra_test::vectors::BLOCKS.iter() {
        transactions.extend(Block::zcash_deserialize(&bytes[..]).unwrap().transactions);
    }
    for upgrade in [NetworkUpgrade::Nu6_3, NetworkUpgrade::Nu7] {
        let branch = upgrade.branch_id().unwrap().try_into().unwrap();
        for orchard in [false, true] {
            for ironwood in [false, true] {
                transactions.push(Arc::new(fake_v6_transaction(
                    upgrade,
                    orchard.then(|| {
                        fake_bundle_for_branch(branch, ::orchard::ValuePool::Orchard, 2, 0).unwrap()
                    }),
                    ironwood.then(|| {
                        fake_bundle_for_branch(branch, ::orchard::ValuePool::Ironwood, 2, 1)
                            .unwrap()
                    }),
                )));
            }
        }
    }
    transactions
}

#[test]
fn decoded_transaction_sizes_match_independent_serialization() {
    let _init_guard = zebra_test::init();
    let mut versions = std::collections::HashSet::new();
    for transaction in transactions() {
        let bytes = transaction.zcash_serialize_to_vec().unwrap();
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0; 8]);
        let decoded = Transaction::zcash_deserialize(&padded[..]).unwrap();
        assert_eq!(decoded.zcash_serialized_size(), bytes.len());
        assert_eq!(decoded.zcash_serialize_to_vec().unwrap(), bytes);
        assert_size(&decoded);
        let unmeasured = Transaction::from_inner(decoded.0.clone());
        assert_eq!(decoded, unmeasured);
        assert_eq!(decoded.hash(), unmeasured.hash());
        assert_eq!(decoded.unmined_id(), unmeasured.unmined_id());
        assert_size(&unmeasured);
        versions.insert(decoded.version());

        // A nonminimal input-count prefix must not become cached canonical-size evidence.
        #[allow(unreachable_patterns)]
        let offset = match decoded.tx_version() {
            TxVersion::Sprout(_) => 4,
            TxVersion::V3 | TxVersion::V4 => 8,
            TxVersion::V5 | TxVersion::V6 => 20,
            _ => panic!("unsupported fixture transaction version"),
        };
        if bytes[offset] < 253 {
            let mut nonminimal = bytes[..offset].to_vec();
            nonminimal.extend_from_slice(&[253, bytes[offset], 0]);
            nonminimal.extend_from_slice(&bytes[offset + 1..]);
            assert!(Transaction::zcash_deserialize(&nonminimal[..]).is_err());
        }
    }
    assert!((1..=6).all(|version| versions.contains(&version)));
}

#[test]
fn rebuilt_transaction_sizes_do_not_reuse_decoded_lengths() {
    let _init_guard = zebra_test::init();
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();
    let transaction = &block.transactions[0];
    let original_size = transaction.zcash_serialized_size();
    let mut outputs = transaction.outputs();
    outputs[0].lock_script = transparent::Script::new(&[0; 300]);
    let changed = transaction
        .as_ref()
        .clone()
        .with_transparent_outputs(outputs.clone());
    assert_ne!(changed.zcash_serialized_size(), original_size);
    assert_size(&changed);
    let mut changed = transaction.as_ref().clone();
    changed.set_outputs(outputs);
    assert_ne!(changed.zcash_serialized_size(), original_size);
    assert_size(&changed);

    let mut inputs = transaction.inputs();
    inputs.push(inputs[0].clone());
    let changed = transaction.as_ref().clone().with_transparent_inputs(inputs);
    assert!(changed.zcash_serialized_size() > original_size);
    assert_size(&changed);

    for transaction in transactions().into_iter().filter(|tx| tx.version() >= 3) {
        let mut changed = transaction.as_ref().clone();
        changed.set_expiry_height(block::Height(1234));
        assert_size(&changed);
        let upgrade = changed.network_upgrade().unwrap_or(NetworkUpgrade::Canopy);
        changed.set_network_upgrade(upgrade);
        assert_size(&changed);
        if transaction.version() == 5 && transaction.has_orchard_shielded_data() {
            let changed = transaction.as_ref().clone().with_orchard_bundle(None);
            assert!(changed.zcash_serialized_size() < transaction.zcash_serialized_size());
            assert_size(&changed);
        }
    }
}
