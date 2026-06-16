//! Tests for the snapshot-consume (assumeUTXO) finalized write path.
//!
//! Covers the direct note-commitment-tree write: a checkpoint block committed
//! with pre-fetched trees supplied writes those trees directly into the tree
//! column families instead of folding them, and the on-disk result matches a
//! normal folded commit.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{service::finalized_state::FinalizedState, CheckpointVerifiedBlock, Config};

/// Commits the mainnet genesis and block 1 to a fresh finalized state and
/// returns it together with block 1.
fn fresh_state_with_genesis(network: &Network) -> (FinalizedState, Arc<Block>) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis deserializes");
    let block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes");

    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis).into(),
            None,
            "snapshot-consume test genesis",
        )
        .expect("genesis commits");

    (state, block1)
}

/// Supplied-tree write round-trip: committing block 1 with its note commitment
/// trees supplied writes the same on-disk trees as folding them normally.
#[test]
fn supplied_tree_write_round_trips() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Baseline: commit block 1 normally (folding the trees).
    let (mut baseline_state, block1) = fresh_state_with_genesis(&network);
    let (_hash, folded_trees) = baseline_state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block1.clone()).into(),
            None,
            "baseline folded commit",
        )
        .expect("block 1 commits by folding");

    let baseline_sapling = baseline_state
        .db
        .sapling_tree_by_height(&Height(1))
        .expect("baseline sapling tree at height 1");
    let baseline_orchard = baseline_state
        .db
        .orchard_tree_by_height(&Height(1))
        .expect("baseline orchard tree at height 1");

    // Supplied: commit the same block 1, but supply the trees directly. The
    // result must match the folded trees byte-for-byte.
    let (mut supplied_state, block1) = fresh_state_with_genesis(&network);
    supplied_state
        .commit_finalized_direct_with_trees(
            CheckpointVerifiedBlock::from(block1).into(),
            None,
            Some(folded_trees.clone()),
            "supplied tree commit",
        )
        .expect("block 1 commits with supplied trees");

    let supplied_sapling = supplied_state
        .db
        .sapling_tree_by_height(&Height(1))
        .expect("supplied sapling tree at height 1");
    let supplied_orchard = supplied_state
        .db
        .orchard_tree_by_height(&Height(1))
        .expect("supplied orchard tree at height 1");

    assert_eq!(
        baseline_sapling.root(),
        supplied_sapling.root(),
        "supplied sapling tree root matches the folded tree",
    );
    assert_eq!(
        baseline_orchard.root(),
        supplied_orchard.root(),
        "supplied orchard tree root matches the folded tree",
    );
    assert_eq!(
        baseline_sapling, supplied_sapling,
        "supplied sapling tree is written byte-identically",
    );
    assert_eq!(
        baseline_orchard, supplied_orchard,
        "supplied orchard tree is written byte-identically",
    );

    // Both states reached the same tip.
    assert_eq!(
        baseline_state.db.finalized_tip_hash(),
        supplied_state.db.finalized_tip_hash(),
        "both commits reach the same tip",
    );
}
