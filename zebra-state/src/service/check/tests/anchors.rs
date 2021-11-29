use std::{convert::TryInto, ops::Deref, sync::Arc};

use zebra_chain::{
    block::{Block, Height},
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
};

use crate::{
    arbitrary::Prepare,
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
};

// sapling

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
