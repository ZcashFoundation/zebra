//! Fixed database test vectors for blocks and transactions.
//!
//! These tests check that the database correctly serializes
//! and deserializes large heights, blocks and transactions.
//!
//! # TODO
//!
//! Test large blocks and transactions with shielded data,
//! including data activated in Overwinter and later network upgrades.

use zebra_chain::{
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
            large_single_transaction_block_many_outputs,
        },
        Block,
    },
    parameters::Network::{self, *},
    serialization::ZcashDeserializeInto,
};
use zebra_test::vectors::{MAINNET_BLOCKS, TESTNET_BLOCKS};

use crate::{service::finalized_state::FinalizedState, Config};

/// Storage round-trip test for block and transaction data in the finalized state database.
#[test]
fn test_block_db_round_trip() {
    let mainnet_test_cases = MAINNET_BLOCKS
        .iter()
        .map(|(_height, block)| block.zcash_deserialize_into().unwrap());
    let testnet_test_cases = TESTNET_BLOCKS
        .iter()
        .map(|(_height, block)| block.zcash_deserialize_into().unwrap());

    let large_block_test_cases = [
        large_multi_transaction_block(),
        large_single_transaction_block_many_inputs(),
        large_single_transaction_block_many_outputs(),
    ];

    test_block_db_round_trip_with(Mainnet, mainnet_test_cases);
    test_block_db_round_trip_with(Testnet, testnet_test_cases);

    // It doesn't matter if these blocks are mainnet or testnet,
    // because there is no validation at this level of the database.
    test_block_db_round_trip_with(Mainnet, large_block_test_cases);
}

fn test_block_db_round_trip_with(
    network: Network,
    block_test_cases: impl IntoIterator<Item = Block>,
) {
    zebra_test::init();

    let mut state = FinalizedState::new(&Config::ephemeral(), network);

    for block in block_test_cases.into_iter() {}
}
