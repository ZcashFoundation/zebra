use std::sync::Arc;

use crate::{
    block::{
        Block,
        Commitment::{self, ChainHistoryActivationReserved},
    },
    history_tree::NonEmptyHistoryTree,
    parameters::{Network, NetworkUpgrade},
    sapling,
    serialization::ZcashDeserializeInto,
};

use color_eyre::eyre;
use eyre::Result;

/// Test the history tree using the activation block of a network upgrade
/// and its next block.
///
/// This test is very similar to the zcash_history test in
/// zebra-chain/src/primitives/zcash_history/tests/vectors.rs, but with the
/// higher level API.
#[test]
fn push_and_prune() -> Result<()> {
    for network in Network::iter() {
        push_and_prune_for_network_upgrade(network.clone(), NetworkUpgrade::Heartwood)?;
        push_and_prune_for_network_upgrade(network, NetworkUpgrade::Canopy)?;
    }
    Ok(())
}

fn push_and_prune_for_network_upgrade(
    network: Network,
    network_upgrade: NetworkUpgrade,
) -> Result<()> {
    let (blocks, sapling_roots) = network.block_sapling_roots_map();

    let height = network_upgrade.activation_height(&network).unwrap().0;

    // Load first block (activation block of the given network upgrade)
    let first_block = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Check its commitment
    let first_commitment = first_block.commitment(&network)?;
    if network_upgrade == NetworkUpgrade::Heartwood {
        // Heartwood is the only upgrade that has a reserved value.
        // (For other upgrades we could compare with the expected commitment,
        // but we haven't calculated them.)
        assert_eq!(first_commitment, ChainHistoryActivationReserved);
    }

    // Build initial history tree with only the first block
    let first_sapling_root =
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("test vector exists"))?;
    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        first_block,
        &first_sapling_root,
        &Default::default(),
    )?;

    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);

    // Compute root hash of the history tree, which will be included in the next block
    let first_root = tree.hash();

    // Load second block (activation + 1)
    let second_block = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Check its commitment
    let second_commitment = second_block.commitment(&network)?;
    assert_eq!(second_commitment, Commitment::ChainHistoryRoot(first_root));

    // Append second block to history tree
    let second_sapling_root = sapling::tree::Root::try_from(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    )?;
    tree.push(second_block, &second_sapling_root, &Default::default())
        .unwrap();

    // Adding a second block will produce a 3-node tree (one parent and two leaves).
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
    for network in Network::iter() {
        upgrade_for_network_upgrade(network, NetworkUpgrade::Canopy)?;
    }
    Ok(())
}

fn upgrade_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) -> Result<()> {
    let (blocks, sapling_roots) = network.block_sapling_roots_map();

    let height = network_upgrade.activation_height(&network).unwrap().0;

    // Load previous block (the block before the activation block of the given network upgrade)
    let block_prev = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Build a history tree with only the previous block (activation height - 1)
    // This tree will not match the actual tree (which has all the blocks since the previous
    // network upgrade), so we won't be able to check if its root is correct.
    let sapling_root_prev =
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("test vector exists"))?;
    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        block_prev,
        &sapling_root_prev,
        &Default::default(),
    )?;

    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height - 1);

    // Load block of the activation height
    let activation_block = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Append block to history tree. This must trigger a upgrade of the tree,
    // which should be recreated.
    let activation_sapling_root = sapling::tree::Root::try_from(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    )?;
    tree.push(
        activation_block,
        &activation_sapling_root,
        &Default::default(),
    )
    .unwrap();

    // Check if the tree has a single node, i.e. it has been recreated.
    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);

    Ok(())
}
