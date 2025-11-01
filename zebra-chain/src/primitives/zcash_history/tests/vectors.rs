use crate::{
    block::Commitment::{self, ChainHistoryActivationReserved},
    serialization::ZcashDeserializeInto,
};

use crate::primitives::zcash_history::*;
use color_eyre::eyre;
use eyre::Result;

/// Test the MMR tree using the activation block of a network upgrade
/// and its next block.
#[test]
fn tree() -> Result<()> {
    for network in Network::iter() {
        tree_for_network_upgrade(&network, NetworkUpgrade::Heartwood)?;
        tree_for_network_upgrade(&network, NetworkUpgrade::Canopy)?;
    }
    Ok(())
}

fn tree_for_network_upgrade(network: &Network, network_upgrade: NetworkUpgrade) -> Result<()> {
    let (blocks, sapling_roots) = network.block_sapling_roots_map();

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

    // Build initial MMR tree with only Block 0
    let sapling_root0 =
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("test vector exists"))?;
    let (mut tree, _) =
        Tree::<V1>::new_from_block(network, block0, &sapling_root0, &Default::default())?;

    // Compute root hash of the MMR tree, which will be included in the next block
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

    // Append Block to MMR tree
    let sapling_root1 = sapling::tree::Root::try_from(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    )?;
    let append = tree
        .append_leaf(block1, &sapling_root1, &Default::default())
        .unwrap();

    // Tree how has 3 nodes: two leaves for each block, and one parent node
    // which is the new root
    assert_eq!(tree.inner.len(), 3);
    // Two nodes were appended: the new leaf and the parent node
    assert_eq!(append.len(), 2);

    Ok(())
}
