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
    let (mut tree, _) = Tree::<V1>::new_from_block(
        network,
        block0,
        BlockCommitmentTreeRoots {
            sapling: &sapling_root0,
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )?;

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
        .append_leaf(
            block1,
            BlockCommitmentTreeRoots {
                sapling: &sapling_root1,
                orchard: &Default::default(),
                ironwood: &Default::default(),
            },
        )
        .unwrap();

    // Tree how has 3 nodes: two leaves for each block, and one parent node
    // which is the new root
    assert_eq!(tree.inner.len(), 3);
    // Two nodes were appended: the new leaf and the parent node
    assert_eq!(append.len(), 2);

    Ok(())
}

/// Network upgrades without a consensus branch ID cannot have history tree
/// nodes, so the tree constructors must return an error instead of panicking.
///
/// `BeforeOverwinter` has no branch ID in any build, which exercises the same
/// code path as a post-Heartwood upgrade missing from `CONSENSUS_BRANCH_IDS`
/// (for example, NU7 in builds without its branch ID).
#[test]
fn constructors_reject_missing_branch_ids() -> Result<()> {
    let network = Network::Mainnet;

    assert!(
        matches!(
            Tree::<V1>::new_from_cache(
                &network,
                NetworkUpgrade::BeforeOverwinter,
                1,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidInput)
        ),
        "a branchless upgrade must fail before cache decoding"
    );

    // The genesis block's network upgrade also has no branch ID.
    let (blocks, _) = network.block_sapling_roots_map();
    let block = Arc::new(
        blocks
            .get(&0)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );
    assert!(
        matches!(
            Tree::<V1>::new_from_block(
                &network,
                block,
                BlockCommitmentTreeRoots {
                    sapling: &Default::default(),
                    orchard: &Default::default(),
                    ironwood: &Default::default(),
                },
            )
            .map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidInput)
        ),
        "a branchless upgrade must fail before building a leaf"
    );

    Ok(())
}
