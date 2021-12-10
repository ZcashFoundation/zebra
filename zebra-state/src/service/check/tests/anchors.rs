//! Tests for whether cited anchors are checked properly.

use std::{convert::TryInto, ops::Deref, sync::Arc};

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    primitives::Groth16Proof,
    serialization::ZcashDeserializeInto,
    sprout::JoinSplit,
    transaction::{JoinSplitData, LockTime, Transaction},
};

use crate::{
    arbitrary::Prepare,
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
    PreparedBlock,
};

// Sprout

/// Check that, when primed with the first blocks that contain Sprout anchors, a
/// Sprout Spend's referenced anchor is validated.
#[test]
fn check_sprout_anchors() {
    zebra_test::init();

    let (mut state, _genesis) = new_state_with_mainnet_genesis();

    // Bootstrap a block at height == 1.
    let block_1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Bootstrap a block just before the first Sprout anchors.
    let block_395 = zebra_test::vectors::BLOCK_MAINNET_395_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Add initial transactions to [`block_1`].
    let block_1 = prepare_sprout_block(block_1, block_395);

    // Validate and commit [`block_1`]. This will add an anchor referencing the
    // empty note commitment tree to the state.
    assert!(state.validate_and_commit(block_1).is_ok());

    // Bootstrap a block at height == 2 that references the Sprout note commitment tree state
    // from [`block_1`].
    let block_2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Exercise Sprout anchor checking with the first shielded transactions with
    // anchors.
    let block_396 = zebra_test::vectors::BLOCK_MAINNET_396_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Add the transactions with the first anchors to [`block_2`].
    let block_2 = prepare_sprout_block(block_2, block_396);

    // Validate and commit [`block_2`]. This will also check the anchors.
    assert_eq!(state.validate_and_commit(block_2), Ok(()));
}

fn prepare_sprout_block(mut block_to_prepare: Block, reference_block: Block) -> PreparedBlock {
    // Convert the coinbase transaction to a version that the non-finalized state will accept.
    block_to_prepare.transactions[0] =
        transaction_v4_from_coinbase(&block_to_prepare.transactions[0]).into();

    reference_block
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sprout_joinsplit_data())
        .for_each(|tx| {
            let joinsplit_data = match tx.deref() {
                Transaction::V2 { joinsplit_data, .. } => joinsplit_data.clone(),
                _ => unreachable!("These are known v2 transactions"),
            };

            // Change [`joinsplit_data`] so that the transaction passes the
            // semantic validation. Namely, set the value balance to zero, and
            // use a dummy Groth16 proof instead of a BCTV14 one.
            let joinsplit_data = joinsplit_data.map(|s| {
                let mut new_joinsplits: Vec<JoinSplit<Groth16Proof>> = Vec::new();

                for old_joinsplit in s.joinsplits() {
                    new_joinsplits.push(JoinSplit {
                        vpub_old: Amount::zero(),
                        vpub_new: Amount::zero(),
                        anchor: old_joinsplit.anchor,
                        nullifiers: old_joinsplit.nullifiers,
                        commitments: old_joinsplit.commitments,
                        ephemeral_key: old_joinsplit.ephemeral_key,
                        random_seed: old_joinsplit.random_seed.clone(),
                        vmacs: old_joinsplit.vmacs.clone(),
                        zkproof: Groth16Proof::from([0; 192]),
                        enc_ciphertexts: old_joinsplit.enc_ciphertexts,
                    })
                }

                match new_joinsplits.split_first() {
                    None => unreachable!("the new joinsplits are never empty"),

                    Some((first, rest)) => JoinSplitData {
                        first: first.clone(),
                        rest: rest.to_vec(),
                        pub_key: s.pub_key,
                        sig: s.sig,
                    },
                }
            });

            // Add the new adjusted transaction to [`block_to_prepare`].
            block_to_prepare
                .transactions
                .push(Arc::new(Transaction::V4 {
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    lock_time: LockTime::min_lock_time(),
                    expiry_height: Height(0),
                    joinsplit_data,
                    sapling_shielded_data: None,
                }))
        });

    Arc::new(block_to_prepare).prepare()
}

// Sapling

/// Check that, when primed with the first Sapling blocks, a Sapling Spend's referenced anchor is
/// validated.
#[test]
fn check_sapling_anchors() {
    zebra_test::init();

    let (mut state, _genesis) = new_state_with_mainnet_genesis();

    // Bootstrap a block at height == 1 that has the first Sapling note commitments
    let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // convert the coinbase transaction to a version that the non-finalized state will accept
    block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

    // Prime finalized state with the Sapling start + 1, which has the first
    // Sapling note commitment
    let block_419201 = zebra_test::vectors::BLOCK_MAINNET_419201_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    block_419201
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sapling_shielded_data())
        .for_each(|tx| {
            let sapling_shielded_data = match tx.deref() {
                Transaction::V4 {
                    sapling_shielded_data,
                    ..
                } => sapling_shielded_data.clone(),
                _ => unreachable!("These are known v4 transactions"),
            };

            // set value balance to 0 to pass the chain value pool checks
            let sapling_shielded_data = sapling_shielded_data.map(|mut s| {
                s.value_balance = 0.try_into().expect("unexpected invalid zero amount");
                s
            });

            block1.transactions.push(Arc::new(Transaction::V4 {
                inputs: Vec::new(),
                outputs: Vec::new(),
                lock_time: LockTime::min_lock_time(),
                expiry_height: Height(0),
                joinsplit_data: None,
                sapling_shielded_data,
            }))
        });

    let block1 = Arc::new(block1).prepare();
    assert!(state.validate_and_commit(block1).is_ok());

    // Bootstrap a block at height == 2 that references the Sapling note commitment tree state
    // from earlier block
    let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // convert the coinbase transaction to a version that the non-finalized state will accept
    block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

    // Exercise Sapling anchor checking with Sapling start + 2, which refers to the note commitment
    // tree as of the last transaction of the previous block
    let block_419202 = zebra_test::vectors::BLOCK_MAINNET_419202_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    block_419202
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sapling_shielded_data())
        .for_each(|tx| {
            let sapling_shielded_data = match tx.deref() {
                Transaction::V4 {
                    sapling_shielded_data,
                    ..
                } => (sapling_shielded_data.clone()),
                _ => unreachable!("These are known v4 transactions"),
            };

            // set value balance to 0 to pass the chain value pool checks
            let sapling_shielded_data = sapling_shielded_data.map(|mut s| {
                s.value_balance = 0.try_into().expect("unexpected invalid zero amount");
                s
            });

            block2.transactions.push(Arc::new(Transaction::V4 {
                inputs: Vec::new(),
                outputs: Vec::new(),
                lock_time: LockTime::min_lock_time(),
                expiry_height: Height(0),
                joinsplit_data: None,
                sapling_shielded_data,
            }))
        });

    let block2 = Arc::new(block2).prepare();
    assert_eq!(state.validate_and_commit(block2), Ok(()));
}
