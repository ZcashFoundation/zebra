//! Utility functions for RPC method tests.

use std::sync::Arc;
use zebra_chain::{
    block::Block,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::Network,
    sapling::tree::Root,
    serialization::ZcashDeserialize,
};

use zebra_test::vectors;

/// Create a history tree with one single block for a network by using Zebra test vectors.
pub fn fake_history_tree(network: Network) -> Arc<HistoryTree> {
    let (block, sapling_root) = match network {
        Network::Mainnet => (
            &vectors::BLOCK_MAINNET_1046400_BYTES[..],
            *vectors::SAPLING_FINAL_ROOT_MAINNET_1046400_BYTES,
        ),
        Network::Testnet => (
            &vectors::BLOCK_TESTNET_1116000_BYTES[..],
            *vectors::SAPLING_FINAL_ROOT_TESTNET_1116000_BYTES,
        ),
    };

    let block = Arc::<Block>::zcash_deserialize(block).expect("block should deserialize");
    let first_sapling_root = Root::try_from(sapling_root).unwrap();

    let history_tree = NonEmptyHistoryTree::from_block(
        Network::Mainnet,
        block,
        &first_sapling_root,
        &Default::default(),
    )
    .unwrap();

    Arc::new(HistoryTree::from(history_tree))
}
