//! Tests for fast checkpoint sync with peer-supplied note commitment tree roots.

mod commit;

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    history_tree::HistoryTree,
    orchard,
    parallel::tree::TreeRoots,
    parameters::Network,
    sapling,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
};
use zebra_test::vectors::{MAINNET_BLOCKS, MAINNET_FINAL_SAPLING_ROOTS};

use crate::CheckpointVerifiedBlock;

use super::*;

/// Returns embedded trees at `height` with empty trees.
fn empty_trees(height: u32) -> Arc<EmbeddedTrees> {
    Arc::new(EmbeddedTrees {
        height: Height(height),
        sapling: Default::default(),
        orchard: Default::default(),
        ironwood: Default::default(),
        sapling_subtrees: Vec::new(),
        orchard_subtrees: Vec::new(),
        ironwood_subtrees: Vec::new(),
    })
}

/// Returns `count` copies of the empty roots.
fn roots(count: usize) -> Vec<TreeRoots> {
    vec![TreeRoots::empty(); count]
}

#[test]
fn cache_keeps_only_the_window_up_to_the_embedded_trees() {
    let cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));

    cache.supply(Height(50), roots(100));
    assert_eq!(cache.roots(Height(99)), None, "below the writer");
    assert!(cache.roots(Height(100)).is_some());
    assert!(cache.roots(Height(149)).is_some());

    cache.supply(Height(990), roots(20));
    assert!(cache.roots(Height(1_000)).is_some());
    assert_eq!(cache.roots(Height(1_001)), None, "above the embedded trees");

    // The window ends `TREE_ROOTS_WINDOW` heights after the writer.
    let cache = TreeRootsCache::new(Some(empty_trees(100_000)), Height(0));
    cache.supply(Height(TREE_ROOTS_WINDOW - 1), roots(2));
    assert!(cache.roots(Height(TREE_ROOTS_WINDOW - 1)).is_some());
    assert_eq!(cache.roots(Height(TREE_ROOTS_WINDOW)), None);
}

#[test]
fn cache_wants_the_first_missing_run() {
    let cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    assert_eq!(cache.wanted(), Some(Height(100)..=Height(1_000)));

    cache.supply(Height(100), roots(10));
    cache.supply(Height(200), roots(10));
    assert_eq!(cache.wanted(), Some(Height(110)..=Height(199)));

    // A rejected root drops the roots after it too, because they usually came in one response.
    cache.reject(Height(105));
    assert_eq!(cache.wanted(), Some(Height(105)..=Height(1_000)));
    assert!(cache.roots(Height(104)).is_some());
    assert_eq!(cache.roots(Height(200)), None);

    cache.supply(Height(105), roots(896));
    assert_eq!(
        cache.wanted(),
        None,
        "every height up to the embedded trees is supplied"
    );

    let cache =
        TreeRootsCache::new(Some(empty_trees(1_000)), Height(0)).with_first_height(Height(500));
    assert_eq!(
        cache.wanted(),
        Some(Height(500)..=Height(1_000)),
        "fast sync can't use roots below its first height"
    );
}

#[test]
fn cache_finishes_after_the_embedded_trees() {
    let cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    cache.supply(Height(900), roots(101));

    cache.set_next_height(Height(950));
    assert_eq!(
        cache.roots(Height(949)),
        None,
        "committed roots are dropped"
    );
    assert!(cache.roots(Height(950)).is_some());
    assert!(cache.embedded_trees().is_some());

    cache.set_next_height(Height(1_001));
    assert!(cache.embedded_trees().is_none());
    assert_eq!(cache.wanted(), None);

    let cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(1_001));
    assert!(
        cache.embedded_trees().is_none(),
        "a state past the embedded trees doesn't fast sync"
    );
}

/// Returns a Mainnet block from the test vectors.
fn mainnet_block(height: u32) -> CheckpointVerifiedBlock {
    let block: Arc<Block> = MAINNET_BLOCKS[&height]
        .zcash_deserialize_into()
        .expect("test vectors are valid blocks");
    CheckpointVerifiedBlock::from(block)
}

/// Returns the Mainnet roots after a block from the test vectors, before NU5.
fn mainnet_roots(height: u32) -> TreeRoots {
    TreeRoots {
        sapling: sapling::tree::Root::try_from(*MAINNET_FINAL_SAPLING_ROOTS[&height])
            .expect("test vectors are valid roots"),
        ..TreeRoots::empty()
    }
}

/// Returns an Orchard root that isn't the empty root.
fn non_empty_orchard_root() -> orchard::tree::Root {
    let mut one = [0; 32];
    one[0] = 1;
    let root = orchard::tree::Root::try_from(one).expect("1 is a valid field element");
    assert_ne!(root, TreeRoots::empty().orchard);
    root
}

#[test]
fn roots_before_heartwood_are_verified_by_their_own_header() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let history_tree = Arc::new(HistoryTree::default());
    let block = mainnet_block(419_201);
    let next = mainnet_block(419_202);

    verify_tree_roots(
        &network,
        &block,
        &mainnet_roots(419_201),
        &history_tree,
        &next,
    )
    .expect("the chain's roots are valid");

    let wrong_sapling = mainnet_roots(419_200);
    assert_ne!(wrong_sapling, mainnet_roots(419_201));
    assert!(matches!(
        verify_tree_roots(&network, &block, &wrong_sapling, &history_tree, &next),
        Err(TreeRootsError::SaplingRoot { .. })
    ));

    let early_orchard = TreeRoots {
        orchard: non_empty_orchard_root(),
        ..mainnet_roots(419_201)
    };
    assert!(matches!(
        verify_tree_roots(&network, &block, &early_orchard, &history_tree, &next),
        Err(TreeRootsError::InactivePool {
            pool: "Orchard",
            ..
        })
    ));
}

#[test]
fn roots_from_heartwood_are_verified_by_the_next_header() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    // The history tree starts with the Heartwood activation block.
    let empty_history = Arc::new(HistoryTree::default());
    let block = mainnet_block(903_000);
    let next = mainnet_block(903_001);

    let history_tree = verify_tree_roots(
        &network,
        &block,
        &mainnet_roots(903_000),
        &empty_history,
        &next,
    )
    .expect("the chain's roots are valid");
    assert!(history_tree.hash().is_some());

    let wrong_sapling = mainnet_roots(902_999);
    assert_ne!(wrong_sapling, mainnet_roots(903_000));
    assert!(matches!(
        verify_tree_roots(&network, &block, &wrong_sapling, &empty_history, &next),
        Err(TreeRootsError::Commitment { .. })
    ));
}

#[test]
fn embedded_trees_round_trip_and_check_against_the_chain() {
    let _init_guard = zebra_test::init();
    let mut sapling = sapling::tree::NoteCommitmentTree::default();
    for _ in 0..3 {
        let cm_u = sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&[0; 32])
            .expect("zero is a valid commitment");
        sapling.append(cm_u).expect("the tree has room");
    }
    let roots = TreeRoots {
        sapling: sapling.root(),
        ..TreeRoots::empty()
    };
    let trees = EmbeddedTrees {
        height: Height(1_234),
        sapling: Arc::new(sapling),
        orchard: Default::default(),
        ironwood: Default::default(),
        sapling_subtrees: Vec::new(),
        orchard_subtrees: Vec::new(),
        ironwood_subtrees: Vec::new(),
    };

    let decoded = EmbeddedTrees::from_bytes(&trees.to_bytes()).expect("serialized trees decode");
    assert_eq!(decoded, trees);

    decoded
        .check(&roots, [3, 0, 0])
        .expect("the trees match their roots and sizes");
    assert!(decoded.check(&roots, [4, 0, 0]).is_err(), "wrong size");
    assert!(
        decoded.check(&TreeRoots::empty(), [3, 0, 0]).is_err(),
        "wrong root"
    );

    let mut bytes = trees.to_bytes();
    bytes.push(0);
    assert!(EmbeddedTrees::from_bytes(&bytes).is_err(), "trailing bytes");
    assert!(
        EmbeddedTrees::from_bytes(&trees.to_bytes()[..10]).is_err(),
        "truncated"
    );
}

#[test]
fn embedded_subtrees_are_one_per_full_subtree_in_order() {
    let subtree = |index: u16| NoteCommitmentSubtree::new(index, Height(1), ());
    let full = 1 << 16;

    assert!(subtrees_are_complete::<()>(&[], full - 1));
    assert!(subtrees_are_complete(&[subtree(0)], full));
    assert!(subtrees_are_complete(
        &[subtree(0), subtree(1)],
        2 * full + 5
    ));
    assert!(!subtrees_are_complete::<()>(&[], full), "a missing subtree");
    assert!(
        !subtrees_are_complete(&[subtree(0), subtree(1)], full),
        "an extra subtree"
    );
    assert!(
        !subtrees_are_complete(&[subtree(1), subtree(0)], 2 * full),
        "out of order"
    );
}
