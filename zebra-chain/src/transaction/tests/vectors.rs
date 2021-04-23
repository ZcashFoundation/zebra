use super::super::*;

use crate::{
    block::{Block, MAX_BLOCK_BYTES},
    sapling::{PerSpendAnchor, SharedAnchor},
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use itertools::Itertools;

use std::convert::TryInto;

#[test]
fn librustzcash_tx_deserialize_and_round_trip() {
    zebra_test::init();

    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

#[test]
fn librustzcash_tx_hash() {
    zebra_test::init();

    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    // TxID taken from comment in zebra_test::vectors
    let hash = tx.hash();
    let expected = "64f0bd7fe30ce23753358fe3a2dc835b8fba9c0274c4e2c54a6f73114cb55639"
        .parse::<Hash>()
        .expect("hash should parse correctly");

    assert_eq!(hash, expected);
}

#[test]
fn zip143_deserialize_and_round_trip() {
    zebra_test::init();

    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_1[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_2[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_2[..], &data2[..]);
}

#[test]
fn zip243_deserialize_and_round_trip() {
    zebra_test::init();

    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_1[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_2[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_2[..], &data2[..]);

    let tx3 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_3[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data3 = Vec::new();
    tx3.zcash_serialize(&mut data3)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_3[..], &data3[..]);
}

// Transaction V5 test vectors

/// An empty transaction v5, with no Orchard, Sapling, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v5_round_trip() {
    zebra_test::init();

    let tx = Transaction::V5 {
        lock_time: LockTime::min_lock_time(),
        expiry_height: block::Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
    };

    let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
    let tx2 = data
        .zcash_deserialize_into()
        .expect("tx should deserialize");

    assert_eq!(tx, tx2);

    let data2 = tx2
        .zcash_serialize_to_vec()
        .expect("vec serialization is infallible");

    assert_eq!(data, data2, "data must be equal if structs are equal");
}

/// An empty transaction v4, with no Sapling, Sprout, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v4_round_trip() {
    zebra_test::init();

    let tx = Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time(),
        expiry_height: block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
    let tx2 = data
        .zcash_deserialize_into()
        .expect("tx should deserialize");

    assert_eq!(tx, tx2);

    let data2 = tx2
        .zcash_serialize_to_vec()
        .expect("vec serialization is infallible");

    assert_eq!(data, data2, "data must be equal if structs are equal");
}

/// Do a round-trip test on fake v5 transactions created from v4 transactions
/// in the block test vectors.
///
/// Covers Sapling only, Transparent only, and Sapling/Transparent v5
/// transactions.
#[test]
fn fake_v5_round_trip() {
    zebra_test::init();

    for original_bytes in zebra_test::vectors::BLOCKS.iter() {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // skip this block if it only contains v5 transactions,
        // the block round-trip test covers it already
        if original_block
            .transactions
            .iter()
            .all(|trans| matches!(trans.as_ref(), &Transaction::V5 { .. }))
        {
            continue;
        }

        let mut fake_block = original_block.clone();
        fake_block.transactions = fake_block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(transaction_to_fake_v5)
            .map(Into::into)
            .collect();

        // test each transaction
        for (original_tx, fake_tx) in original_block
            .transactions
            .iter()
            .zip(fake_block.transactions.iter())
        {
            assert_ne!(
                &original_tx, &fake_tx,
                "v1-v4 transactions must change when converted to fake v5"
            );

            let fake_bytes = fake_tx
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            assert_ne!(
                &original_bytes[..],
                fake_bytes,
                "v1-v4 transaction data must change when converted to fake v5"
            );

            let fake_tx2 = fake_bytes
                .zcash_deserialize_into::<Transaction>()
                .expect("tx is structurally valid");

            assert_eq!(fake_tx.as_ref(), &fake_tx2);

            let fake_bytes2 = fake_tx2
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            assert_eq!(
                fake_bytes, fake_bytes2,
                "data must be equal if structs are equal"
            );
        }

        // test full blocks
        assert_ne!(
            &original_block, &fake_block,
            "v1-v4 transactions must change when converted to fake v5"
        );

        let fake_bytes = fake_block
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_ne!(
            &original_bytes[..],
            fake_bytes,
            "v1-v4 transaction data must change when converted to fake v5"
        );

        // skip fake blocks which exceed the block size limit
        if fake_bytes.len() > MAX_BLOCK_BYTES.try_into().unwrap() {
            continue;
        }

        let fake_block2 = fake_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        assert_eq!(fake_block, fake_block2);

        let fake_bytes2 = fake_block2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(
            fake_bytes, fake_bytes2,
            "data must be equal if structs are equal"
        );
    }
}

// Utility functions

/// Convert `trans` into a fake v5 transaction,
/// converting sapling shielded data from v4 to v5 if possible.
fn transaction_to_fake_v5(trans: &Transaction) -> Transaction {
    use Transaction::*;

    match trans {
        V1 {
            inputs,
            outputs,
            lock_time,
        } => V5 {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: block::Height(0),
            sapling_shielded_data: None,
        },
        V2 {
            inputs,
            outputs,
            lock_time,
            ..
        } => V5 {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: block::Height(0),
            sapling_shielded_data: None,
        },
        V3 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            ..
        } => V5 {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: *expiry_height,
            sapling_shielded_data: None,
        },
        V4 {
            inputs,
            outputs,
            lock_time,
            expiry_height,
            sapling_shielded_data,
            ..
        } => V5 {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            lock_time: *lock_time,
            expiry_height: *expiry_height,
            sapling_shielded_data: sapling_shielded_data
                .clone()
                .map(sapling_shielded_v4_to_fake_v5)
                .flatten(),
        },
        v5 @ V5 { .. } => v5.clone(),
    }
}

/// Convert a v4 sapling shielded data into a fake v5 sapling shielded data,
/// if possible.
fn sapling_shielded_v4_to_fake_v5(
    v4_shielded: sapling::ShieldedData<PerSpendAnchor>,
) -> Option<sapling::ShieldedData<SharedAnchor>> {
    use sapling::ShieldedData;
    use sapling::TransferData::*;

    let unique_anchors: Vec<_> = v4_shielded
        .spends()
        .map(|spend| spend.per_spend_anchor)
        .unique()
        .collect();

    let fake_spends: Vec<_> = v4_shielded
        .spends()
        .cloned()
        .map(sapling_spend_v4_to_fake_v5)
        .collect();

    let transfers = match v4_shielded.transfers {
        SpendsAndMaybeOutputs { maybe_outputs, .. } => {
            let shared_anchor = match unique_anchors.as_slice() {
                [unique_anchor] => *unique_anchor,
                // Multiple different anchors, can't convert to v5
                _ => return None,
            };

            SpendsAndMaybeOutputs {
                shared_anchor,
                spends: fake_spends.try_into().unwrap(),
                maybe_outputs,
            }
        }
        JustOutputs { outputs } => JustOutputs { outputs },
    };

    let fake_shielded_v5 = ShieldedData::<SharedAnchor> {
        value_balance: v4_shielded.value_balance,
        transfers,
        binding_sig: v4_shielded.binding_sig,
    };

    Some(fake_shielded_v5)
}

/// Convert a v4 sapling spend into a fake v5 sapling spend.
fn sapling_spend_v4_to_fake_v5(
    v4_spend: sapling::Spend<PerSpendAnchor>,
) -> sapling::Spend<SharedAnchor> {
    use sapling::Spend;

    Spend::<SharedAnchor> {
        cv: v4_spend.cv,
        per_spend_anchor: FieldNotPresent,
        nullifier: v4_spend.nullifier,
        rk: v4_spend.rk,
        zkproof: v4_spend.zkproof,
        spend_auth_sig: v4_spend.spend_auth_sig,
    }
}
