use std::sync::Arc;

use crate::{
    block::{
        Block,
        Commitment::{self, ChainHistoryActivationReserved},
    },
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade},
    sapling,
    serialization::ZcashDeserializeInto,
};

use color_eyre::eyre;
use eyre::Result;
use zebra_test::vectors::{
    MAINNET_BLOCKS, MAINNET_FINAL_SAPLING_ROOTS, TESTNET_BLOCKS, TESTNET_FINAL_SAPLING_ROOTS,
};

/// Test the history tree using the activation block of a network upgrade
/// and its next block.
///
/// This test is very similar to the zcash_history test in
/// zebra-chain/src/primitives/zcash_history/tests/vectors.rs, but with the
/// higher level API.
#[test]
fn push_and_prune() -> Result<()> {
    push_and_prune_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Heartwood)?;
    push_and_prune_for_network_upgrade(Network::Testnet, NetworkUpgrade::Heartwood)?;
    push_and_prune_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Canopy)?;
    push_and_prune_for_network_upgrade(Network::Testnet, NetworkUpgrade::Canopy)?;
    Ok(())
}

fn push_and_prune_for_network_upgrade(
    network: Network,
    network_upgrade: NetworkUpgrade,
) -> Result<()> {
    let (blocks, sapling_roots) = match network {
        Network::Mainnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SAPLING_ROOTS),
        Network::Testnet => (&*TESTNET_BLOCKS, &*TESTNET_FINAL_SAPLING_ROOTS),
    };
    let height = network_upgrade.activation_height(network).unwrap().0;

    // Load Block 0 (activation block of the given network upgrade)
    let block0 = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Check its commitment
    let commitment0 = block0.commitment(network)?;
    if network_upgrade == NetworkUpgrade::Heartwood {
        // Heartwood is the only upgrade that has a reserved value.
        // (For other upgrades we could compare with the expected commitment,
        // but we haven't calculated them.)
        assert_eq!(commitment0, ChainHistoryActivationReserved);
    }

    // Build initial history tree tree with only Block 0
    let sapling_root0 =
        sapling::tree::Root(**sapling_roots.get(&height).expect("test vector exists"));
    let mut tree = HistoryTree::from_block(network, block0, &sapling_root0, &Default::default())?;

    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);

    // Compute root hash of the history tree, which will be included in the next block
    let hash0 = tree.hash();

    // Load Block 1 (activation + 1)
    let block1 = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Check its commitment
    let commitment1 = block1.commitment(network)?;
    assert_eq!(commitment1, Commitment::ChainHistoryRoot(hash0));

    // Append Block to history tree
    let sapling_root1 = sapling::tree::Root(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    );
    tree.push(block1, &sapling_root1, &Default::default())
        .unwrap();

    // Adding a second block will produce a 3-node tree (one parent and two leafs).
    assert_eq!(tree.size(), 3);
    // The tree must have been pruned, resulting in a single peak (the parent).
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height + 1);

    Ok(())
}

/// Test the history tree works during a network upgrade using the block
/// of a network upgrade and the previous block from the previous upgrade.
#[test]
fn upgrade() -> Result<()> {
    // The history tree only exists Hearwood-onward, and the only upgrade for which
    // we have vectors since then is Canopy. Therefore, only test the Heartwood->Canopy upgrade.
    upgrade_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Canopy)?;
    upgrade_for_network_upgrade(Network::Testnet, NetworkUpgrade::Canopy)?;
    Ok(())
}

fn upgrade_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) -> Result<()> {
    let (blocks, sapling_roots) = match network {
        Network::Mainnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SAPLING_ROOTS),
        Network::Testnet => (&*TESTNET_BLOCKS, &*TESTNET_FINAL_SAPLING_ROOTS),
    };
    let height = network_upgrade.activation_height(network).unwrap().0;

    // Load Block -1 (the block before the activation block of the given network upgrade)
    let block_prev = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Build a history tree with only Block "-1" (activation height - 1)
    // This tree will not match the actual tree (which has all the blocks since the previous
    // network upgrade), so we won't be able to check if its root is correct.
    let sapling_root_prev =
        sapling::tree::Root(**sapling_roots.get(&height).expect("test vector exists"));
    let mut tree =
        HistoryTree::from_block(network, block_prev, &sapling_root_prev, &Default::default())?;

    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height - 1);

    // Load Block 0 (activation height + 0)
    let block0 = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Append Block to history tree. This must trigger a upgrade of the tree,
    // which should be recreated.
    let sapling_root0 = sapling::tree::Root(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    );
    tree.push(block0, &sapling_root0, &Default::default())
        .unwrap();

    // Check if the tree has a single node, i.e. it has been recreated.
    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);

    Ok(())
}
