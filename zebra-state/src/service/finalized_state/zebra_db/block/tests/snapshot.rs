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

/// Block hash structure for RON snapshots.
///
/// This structure is used to snapshot the height and hash on the same line,
/// which looks good for a vector of heights and hashes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct BlockHash(String);

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

    if let Some((max_height, tip_block_hash)) = tip {
        // Check block height and hash database queries.
        let mut stored_block_hashes = Vec::new();

        for query_height in 0..=max_height.0 {
            let query_height = Height(query_height);
            let stored_block_hash = state
                .hash(query_height)
                .expect("heights up to tip have hashes");
            let stored_height = state
                .height(stored_block_hash)
                .expect("hashes up to tip have heights");

            // We don't need to snapshot the heights,
            // because they are fully determined by the tip and block hashes.
            //
            // But we do it anyway, so the snapshots are more readable.
            assert_eq!(stored_height, query_height);

            if query_height == max_height {
                assert_eq!(stored_block_hash, tip_block_hash);
            }

            stored_block_hashes.push((stored_height, BlockHash(stored_block_hash.to_string())));
        }

        // The block hashes are in height order, and we want to snapshot that order.
        // So we don't sort the vector before snapshotting.
        insta::assert_ron_snapshot!("block_hashes", stored_block_hashes);
    }
}
