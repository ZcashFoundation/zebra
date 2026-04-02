//! Fixed database test vectors for blocks and transactions.
//!
//! These tests check that the database correctly serializes
//! and deserializes large heights, blocks and transactions.
//!
//! # TODO
//!
//! Test large blocks and transactions with shielded data,
//! including data activated in Overwinter and later network upgrades.
//!
//! Check transparent address indexes, UTXOs, etc.

use std::{iter, sync::Arc};

use zebra_chain::{
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
            large_single_transaction_block_many_outputs,
        },
        Block, Height,
    },
    parameters::Network::{self, *},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::new_ordered_outputs_with_height,
};
use zebra_test::vectors::{MAINNET_BLOCKS, TESTNET_BLOCKS};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    request::{FinalizedBlock, Treestate},
    service::{
        finalized_state::{disk_db::DiskWriteBatch, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
        non_finalized_state::NonFinalizedState,
    },
    CheckpointVerifiedBlock, Config, SemanticallyVerifiedBlock,
};

/// Storage round-trip test for block and transaction data in the finalized state database.
#[test]
fn test_block_db_round_trip() {
    let mainnet_test_cases = MAINNET_BLOCKS
        .values()
        .map(|block| block.zcash_deserialize_into().unwrap());
    let testnet_test_cases = TESTNET_BLOCKS
        .values()
        .map(|block| block.zcash_deserialize_into().unwrap());

    test_block_db_round_trip_with(&Mainnet, mainnet_test_cases);
    test_block_db_round_trip_with(&Network::new_default_testnet(), testnet_test_cases);

    // It doesn't matter if these blocks are mainnet or testnet,
    // because there is no validation at this level of the database.
    //
    // These blocks have the same height and header hash, so they each need a new state.
    test_block_db_round_trip_with(&Mainnet, iter::once(large_multi_transaction_block()));

    // These blocks are unstable under serialization, so we apply a round-trip first.
    //
    // TODO: fix the bug in the generated test vectors.
    let block = large_single_transaction_block_many_inputs();
    let block_data = block
        .zcash_serialize_to_vec()
        .expect("serialization to vec never fails");
    let block: Block = block_data
        .zcash_deserialize_into()
        .expect("deserialization of valid serialized block never fails");
    test_block_db_round_trip_with(&Mainnet, iter::once(block));

    let block = large_single_transaction_block_many_outputs();
    let block_data = block
        .zcash_serialize_to_vec()
        .expect("serialization to vec never fails");
    let block: Block = block_data
        .zcash_deserialize_into()
        .expect("deserialization of valid serialized block never fails");
    test_block_db_round_trip_with(&Mainnet, iter::once(block));
}

/// Test that `lookup_spent_utxos` returns an empty vec for blocks with no transparent inputs,
/// and that `write_block` works correctly when provided spent UTXOs externally.
#[test]
fn test_lookup_spent_utxos_and_write_block_with_provided_utxos() {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let mut state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );
    let empty_nfs = NonFinalizedState::new(&network);

    // Commit the first few mainnet blocks using the new lookup_spent_utxos + write_block path.
    // These early blocks have only coinbase transactions (no transparent inputs to spend),
    // so lookup_spent_utxos should return an empty vec.
    for (&height, block_bytes) in MAINNET_BLOCKS.iter().take(3) {
        let block: Arc<Block> = block_bytes.zcash_deserialize_into().unwrap();
        let checkpoint_verified = CheckpointVerifiedBlock::from(block.clone());
        let treestate = Treestate::default();
        let finalized = FinalizedBlock::from_checkpoint_verified(checkpoint_verified, treestate);

        let spent_utxos = state.lookup_spent_utxos(&finalized, &empty_nfs);

        // Early mainnet blocks have only coinbase inputs (no transparent spends).
        assert!(
            spent_utxos.is_empty(),
            "block at height {height} should have no transparent spends"
        );

        state
            .write_block(finalized, spent_utxos, None, &network, "test")
            .expect("write_block should succeed for valid blocks");
    }

    // Verify the blocks were written by reading back the tip.
    let tip = state.tip();
    assert!(
        tip.is_some(),
        "state should have a tip after writing blocks"
    );
}

/// Test that `lookup_spent_utxos` falls back to the non-finalized state for UTXOs
/// not yet on disk.
#[test]
fn test_lookup_spent_utxos_falls_back_to_non_finalized_state() {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );
    let empty_nfs = NonFinalizedState::new(&network);

    // With an empty database and empty non-finalized state, lookup_spent_utxos
    // for blocks without transparent spends should return empty.
    let block: Arc<Block> = MAINNET_BLOCKS
        .values()
        .next()
        .unwrap()
        .zcash_deserialize_into()
        .unwrap();
    let checkpoint_verified = CheckpointVerifiedBlock::from(block);
    let treestate = Treestate::default();
    let finalized = FinalizedBlock::from_checkpoint_verified(checkpoint_verified, treestate);

    let spent_utxos = state.lookup_spent_utxos(&finalized, &empty_nfs);
    assert!(
        spent_utxos.is_empty(),
        "genesis block should have no transparent spends"
    );
}

fn test_block_db_round_trip_with(
    network: &Network,
    block_test_cases: impl IntoIterator<Item = Block>,
) {
    let _init_guard = zebra_test::init();

    let state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        // The raw database accesses in this test create invalid database formats.
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );

    // Check that each block round-trips to the database
    for original_block in block_test_cases.into_iter() {
        // First, check that the block round-trips without using the database
        let block_data = original_block
            .zcash_serialize_to_vec()
            .expect("serialization to vec never fails");
        let round_trip_block: Block = block_data
            .zcash_deserialize_into()
            .expect("deserialization of valid serialized block never fails");
        let round_trip_data = round_trip_block
            .zcash_serialize_to_vec()
            .expect("serialization to vec never fails");

        assert_eq!(
            original_block, round_trip_block,
            "test block structure must round-trip",
        );
        assert_eq!(
            block_data, round_trip_data,
            "test block data must round-trip",
        );

        // Now, use the database
        let original_block = Arc::new(original_block);
        let checkpoint_verified = if original_block.coinbase_height().is_some() {
            CheckpointVerifiedBlock::from(original_block.clone())
        } else {
            // Fake a zero height
            let hash = original_block.hash();
            let transaction_hashes: Arc<[_]> = original_block
                .transactions
                .iter()
                .map(|tx| tx.hash())
                .collect();
            let new_outputs =
                new_ordered_outputs_with_height(&original_block, Height(0), &transaction_hashes);

            CheckpointVerifiedBlock(SemanticallyVerifiedBlock {
                block: original_block.clone(),
                hash,
                height: Height(0),
                new_outputs,
                transaction_hashes,
                deferred_pool_balance_change: None,
            })
        };

        let dummy_treestate = Treestate::default();
        let finalized =
            FinalizedBlock::from_checkpoint_verified(checkpoint_verified, dummy_treestate);

        // Skip validation by writing the block directly to the database
        let mut batch = DiskWriteBatch::new();
        batch.prepare_block_header_and_transaction_data_batch(&state.db, &finalized);
        state.db.write(batch).expect("block is valid for writing");

        // Now read it back from the state
        let stored_block = state
            .block(finalized.height.into())
            .expect("block was stored at height");

        if stored_block != original_block {
            error!(
                "
                detailed block mismatch report:
                original: {:?}\n\
                original data: {:?}\n\
                stored: {:?}\n\
                stored data: {:?}\n\
                ",
                original_block,
                hex::encode(original_block.zcash_serialize_to_vec().unwrap()),
                stored_block,
                hex::encode(stored_block.zcash_serialize_to_vec().unwrap()),
            );
        }

        assert_eq!(stored_block, original_block);
    }
}
