use std::{collections::BTreeMap, sync::Arc};

use crate::{
    block::{
        Block,
        Commitment::{self, ChainHistoryActivationReserved},
        Height,
    },
    history_tree::{HistoryPeak, HistoryTreeError, NonEmptyHistoryTree},
    orchard,
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    primitives::zcash_history::{self, BlockCommitmentTreeRoots},
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
        BlockCommitmentTreeRoots {
            sapling: &first_sapling_root,
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )?;

    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);
    // Heartwood and Canopy trees have V1 nodes, which carry no Orchard or Ironwood root.
    assert_eq!(
        tree.last_peak()?,
        HistoryPeak {
            end_height: Height(height),
            end_sapling_root: (&first_sapling_root).into(),
            end_orchard_root: None,
            end_ironwood_root: None,
        },
    );

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
    tree.push(
        second_block,
        BlockCommitmentTreeRoots {
            sapling: &second_sapling_root,
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .unwrap();

    // Adding a second block will produce a 3-node tree (one parent and two leaves).
    assert_eq!(tree.size(), 3);
    // The tree must have been pruned, resulting in a single peak (the parent).
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height + 1);
    // The parent's end fields are its rightmost leaf's: the second block's.
    assert_eq!(
        tree.last_peak()?,
        HistoryPeak {
            end_height: Height(height + 1),
            end_sapling_root: (&second_sapling_root).into(),
            end_orchard_root: None,
            end_ironwood_root: None,
        },
    );

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
        BlockCommitmentTreeRoots {
            sapling: &sapling_root_prev,
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
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
        BlockCommitmentTreeRoots {
            sapling: &activation_sapling_root,
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .unwrap();

    // Check if the tree has a single node, i.e. it has been recreated.
    assert_eq!(tree.size(), 1);
    assert_eq!(tree.peaks().len(), 1);
    assert_eq!(tree.current_height().0, height);

    Ok(())
}

/// Returns the Mainnet test vector block at `height`.
fn mainnet_block(height: u32) -> Arc<Block> {
    Arc::new(
        zebra_test::vectors::MAINNET_BLOCKS
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    )
}

/// Asserts that `tree` and the same tree rebuilt from its cached peaks both
/// report `expected` as their last peak.
///
/// The rebuilt tree combines its peaks into a new root, so this also checks
/// that the root of a multi-peak tree carries its rightmost peak's fields.
fn assert_last_peak(tree: &NonEmptyHistoryTree, expected: HistoryPeak) -> Result<()> {
    assert_eq!(tree.last_peak()?, expected);

    let rebuilt = NonEmptyHistoryTree::from_cache(
        tree.network(),
        tree.size(),
        tree.peaks().clone(),
        tree.current_height(),
    )?;
    assert_eq!(rebuilt.last_peak()?, expected);
    assert_eq!(rebuilt.hash(), tree.hash());

    Ok(())
}

/// Returns the Orchard root in the Mainnet Orchard root vector `bytes`.
///
/// Those vectors are stored reversed: their hex strings are already in
/// serialized order, and reversing them makes values above the Pallas modulus.
fn orchard_root(bytes: [u8; 32]) -> Result<orchard::tree::Root> {
    let mut bytes = bytes;
    bytes.reverse();
    Ok(orchard::tree::Root::try_from(bytes)?)
}

/// Returns a single-leaf Canopy history tree on Mainnet whose leaf ends at
/// `end_height`.
fn tree_ending_at(end_height: u64) -> Result<NonEmptyHistoryTree> {
    let network = Network::Mainnet;
    let current_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy is active on Mainnet");

    let leaf = <zcash_history::V1 as zcash_history::Version>::block_to_history_node(
        mainnet_block(current_height.0),
        &network,
        BlockCommitmentTreeRoots {
            sapling: &Default::default(),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    );
    let leaf = ::zcash_history::NodeData {
        start_height: end_height,
        end_height,
        ..leaf
    };
    let mut bytes = [0; ::zcash_history::MAX_ENTRY_SIZE];
    ::zcash_history::Entry::<zcash_history::V1>::new_leaf(leaf)
        .write(&mut &mut bytes[..])
        .expect("a leaf entry fits in the maximum entry size");

    Ok(NonEmptyHistoryTree::from_cache(
        &network,
        1,
        BTreeMap::from([(0, zcash_history::Entry::from_raw_bytes_padded(&bytes))]),
        current_height,
    )?)
}

/// NU5 trees have V2 nodes, whose last peak carries the Orchard root as well
/// as the Sapling root, checked with real Mainnet blocks and roots.
#[test]
fn last_peak_reads_the_orchard_root_of_v2_nodes() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (_blocks, sapling_roots) = network.block_sapling_roots_map();
    let orchard_roots = network.orchard_anchors();
    let roots = |height: u32| -> Result<(sapling::tree::Root, orchard::tree::Root)> {
        Ok((
            sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("vector exists"))?,
            orchard_root(**orchard_roots.get(&height).expect("vector exists"))?,
        ))
    };
    let peak = |height: u32| -> Result<HistoryPeak> {
        let (sapling_root, orchard_root) = roots(height)?;
        Ok(HistoryPeak {
            end_height: Height(height),
            end_sapling_root: (&sapling_root).into(),
            end_orchard_root: Some((&orchard_root).into()),
            end_ironwood_root: None,
        })
    };

    // Three consecutive NU5 blocks, the first ones the vectors have.
    let first = 1_687_106;
    assert_eq!(
        NetworkUpgrade::current(&network, Height(first)),
        NetworkUpgrade::Nu5
    );

    let (sapling_root, orchard_root) = roots(first)?;
    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        mainnet_block(first),
        BlockCommitmentTreeRoots {
            sapling: &sapling_root,
            orchard: &orchard_root,
            ironwood: &Default::default(),
        },
    )?;
    assert_last_peak(&tree, peak(first)?)?;

    for height in [first + 1, first + 2] {
        let (sapling_root, orchard_root) = roots(height)?;
        tree.push(
            mainnet_block(height),
            BlockCommitmentTreeRoots {
                sapling: &sapling_root,
                orchard: &orchard_root,
                ironwood: &Default::default(),
            },
        )?;
        assert_last_peak(&tree, peak(height)?)?;
    }

    // Three leaves make two peaks, so the last peak came from a combined root.
    assert_eq!(tree.peaks().len(), 2);

    Ok(())
}

/// NU6.3 trees have V3 nodes, whose last peak also carries the Ironwood root
/// (ZIP 258).
///
/// No NU6.3 block vectors exist, so this uses a Regtest network that activates
/// NU6.3 at height 1, with the real Mainnet blocks 1 to 3, whose nodes only
/// depend on their headers and transaction counts, and a distinct valid root
/// for each pool and height.
#[test]
fn last_peak_reads_the_ironwood_root_of_v3_nodes() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            ..Default::default()
        }
        .into(),
    );
    assert_eq!(
        NetworkUpgrade::current(&network, Height(1)),
        NetworkUpgrade::Nu6_3
    );

    // Valid Sapling and Pallas field elements from the NU5 vectors, at heights whose Orchard roots
    // all differ.
    let (_blocks, sapling_roots) = Network::Mainnet.block_sapling_roots_map();
    let orchard_roots = Network::Mainnet.orchard_anchors();
    let vector_heights = [1_687_106, 1_687_107, 1_687_118, 1_687_121];
    let roots = |height: u32| -> Result<(
        sapling::tree::Root,
        orchard::tree::Root,
        orchard::tree::Root,
    )> {
        let index = usize::try_from(height - 1)?;
        Ok((
            sapling::tree::Root::try_from(
                **sapling_roots
                    .get(&vector_heights[index])
                    .expect("vector exists"),
            )?,
            orchard_root(
                **orchard_roots
                    .get(&vector_heights[index])
                    .expect("vector exists"),
            )?,
            orchard_root(
                **orchard_roots
                    .get(&vector_heights[index + 1])
                    .expect("vector exists"),
            )?,
        ))
    };
    let peak = |height: u32| -> Result<HistoryPeak> {
        let (sapling_root, orchard_root, ironwood_root) = roots(height)?;
        assert_ne!(orchard_root, ironwood_root, "the pools have distinct roots");
        Ok(HistoryPeak {
            end_height: Height(height),
            end_sapling_root: (&sapling_root).into(),
            end_orchard_root: Some((&orchard_root).into()),
            end_ironwood_root: Some((&ironwood_root).into()),
        })
    };
    let block = |height: u32| -> Arc<Block> {
        Arc::new(
            zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                .get(&height)
                .expect("test vector exists")
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid"),
        )
    };

    let (sapling_root, orchard_root, ironwood_root) = roots(1)?;
    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        block(1),
        BlockCommitmentTreeRoots {
            sapling: &sapling_root,
            orchard: &orchard_root,
            ironwood: &ironwood_root,
        },
    )?;
    assert_last_peak(&tree, peak(1)?)?;

    for height in [2, 3] {
        let (sapling_root, orchard_root, ironwood_root) = roots(height)?;
        tree.push(
            block(height),
            BlockCommitmentTreeRoots {
                sapling: &sapling_root,
                orchard: &orchard_root,
                ironwood: &ironwood_root,
            },
        )?;
        assert_last_peak(&tree, peak(height)?)?;
    }
    assert_eq!(tree.peaks().len(), 2);

    Ok(())
}

/// A peak whose end height is not a block height is an error, not a
/// truncated height.
#[test]
fn last_peak_rejects_end_heights_above_the_maximum() -> Result<()> {
    let _init_guard = zebra_test::init();

    for end_height in [
        u64::from(Height::MAX.0) + 1,
        u64::from(u32::MAX),
        u64::from(u32::MAX) + 1,
    ] {
        assert_eq!(
            tree_ending_at(end_height)?.last_peak(),
            Err(HistoryTreeError::InvalidPeak(
                "the end height is above the maximum block height"
            )),
            "{end_height}",
        );
    }

    // The maximum block height itself is valid.
    assert_eq!(
        tree_ending_at(u64::from(Height::MAX.0))?
            .last_peak()?
            .end_height,
        Height::MAX,
    );

    Ok(())
}
