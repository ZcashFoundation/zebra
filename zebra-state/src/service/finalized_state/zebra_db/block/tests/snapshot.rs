//! Database snapshot tests for blocks and transactions.
//!
//! These tests check the values returned by all the APIs in the `zebra_db::block` module,
//! iterating through the possible arguments as needed:
//! - tip
//! - hash(height: tip..=0)
//! - height(hash: heights)
//! - block(height: tip..=0)
//! - transaction_hash(location: { height: tip..=0, index: 0.. })
//! - transaction(hash: hashes)
//!
//! But skips the following trivial variations:
//! - is_empty: covered by `tip == None`
//! - block(hash): covered by `height(hash) and block(height)`
//!
//! These tests use fixed test vectors, based on the results of other database queries.
//!
//! # Fixing Test Failures
//!
//! If this test fails, run `cargo insta review` to update the test snapshots,
//! then commit the `test_*.snap` files using git.
//!
//! # TODO
//!
//! Test shielded data, and data activated in Overwinter and later network upgrades.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network::{self, *},
    serialization::ZcashDeserializeInto,
};

use crate::{service::finalized_state::FinalizedState, Config};

/// Tip structure for RON snapshots.
///
/// This structure snapshots the height and hash on separate lines,
/// which looks good for a single entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Tip {
    height: u32,
    block_hash: String,
}

impl From<(Height, block::Hash)> for Tip {
    fn from((height, hash): (Height, block::Hash)) -> Tip {
        Tip {
            height: height.0,
            block_hash: hash.to_string(),
        }
    }
}

/// Snapshot test for finalized block and transaction data.
#[test]
fn test_block_and_transaction_data() {
    zebra_test::init();

    test_block_and_transaction_data_with_network(Mainnet);
    test_block_and_transaction_data_with_network(Testnet);
}

/// Snapshot finalized block and transaction data for `network`.
///
/// See [`test_block_and_transaction_data`].
fn test_block_and_transaction_data_with_network(network: Network) {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();

    let mut state = FinalizedState::new(&Config::ephemeral(), network);

    // Assert that empty databases are the same, regardless of the network.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix("no_blocks");

    settings.bind(|| snapshot_block_and_transaction_data(&state));

    // Snapshot block and transaction database data for:
    // - mainnet and testnet
    // - genesis, block 1, and block 2
    let blocks = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    // We limit the number of blocks, because the serialized data is a few kilobytes per block.
    for height in 0..=2 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), "snapshot tests")
            .expect("test block is valid");

        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!("{}_{}", net_suffix, height));

        settings.bind(|| snapshot_block_and_transaction_data(&state));
    }
}

/// Snapshot block and transaction data, using `cargo insta` and RON serialization.
fn snapshot_block_and_transaction_data(state: &FinalizedState) {
    let tip = state.tip();

    insta::assert_ron_snapshot!("tip", tip.map(|tip| Tip::from(tip)));
}
