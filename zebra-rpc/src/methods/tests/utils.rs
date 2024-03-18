//! Utility functions for RPC method tests.

use std::sync::Arc;
use zebra_chain::{
    block::Block,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::Network,
    sapling::tree::Root,
    serialization::ZcashDeserialize,
};

/// Create a history tree with one single block for a network by using Zebra test vectors.
pub fn fake_history_tree(network: &Network) -> Arc<HistoryTree> {
    let (block, sapling_root) = network.test_block_sapling_roots(1046400, 1116000).unwrap();

    let block = Arc::<Block>::zcash_deserialize(block).expect("block should deserialize");
    let first_sapling_root = Root::try_from(sapling_root).unwrap();

    let history_tree = NonEmptyHistoryTree::from_block(
        &Network::Mainnet,
        block,
        &first_sapling_root,
        &Default::default(),
    )
    .unwrap();

    Arc::new(HistoryTree::from(history_tree))
}
