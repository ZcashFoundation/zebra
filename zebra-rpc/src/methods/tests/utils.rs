//! Utility functions for RPC method tests.

use std::sync::Arc;
use zebra_chain::{
    block::{Block, ChainHistoryMmrRootHash},
    history_tree::NonEmptyHistoryTree,
    parameters::Network,
    sapling,
    serialization::ZcashDeserialize,
};
use zebra_test::vectors::{
    SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES, SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
};

/// Creates a history tree with a single block for the given network by using Zebra test vectors.
///
/// Returns the tree root, and the Sapling note commitment tree root used for its computation.
pub fn fake_roots(net: &Network) -> (ChainHistoryMmrRootHash, sapling::tree::Root) {
    let (block, sapling_root) = if net.is_mainnet() {
        (
            &zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..],
            *SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES,
        )
    } else {
        (
            &zebra_test::vectors::BLOCK_TESTNET_1116000_BYTES[..],
            *SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
        )
    };

    let block = Arc::<Block>::zcash_deserialize(block).expect("block should deserialize");
    let sapling_root = sapling::tree::Root::try_from(sapling_root).unwrap();

    let hist_root = NonEmptyHistoryTree::from_block(net, block, &sapling_root, &Default::default())
        .expect("should create a fake history tree")
        .hash();

    (hist_root, sapling_root)
}
