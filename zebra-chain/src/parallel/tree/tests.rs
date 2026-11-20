//! Tests for parallel note commitment tree updates.

use std::{collections::BTreeSet, sync::Arc};

use proptest::{
    arbitrary::Arbitrary,
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};

use halo2::pasta::pallas;

use crate::{
    block::{Block, Height},
    orchard,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
    LedgerState,
};

use super::{NoteCommitmentTreeError, NoteCommitmentTrees, PrecomputedTreeUpdate};

/// Updates `trees` one pool at a time, in the order that
/// [`NoteCommitmentTrees::update_trees_parallel`] returns errors.
fn update_trees_in_series(
    trees: &mut NoteCommitmentTrees,
    block: &Block,
) -> Result<(), NoteCommitmentTreeError> {
    let height = block
        .coinbase_height()
        .expect("test vector blocks have a coinbase height");

    let sprout: Vec<_> = block.sprout_note_commitments().collect();
    if !sprout.is_empty() {
        trees.sprout =
            NoteCommitmentTrees::update_sprout_note_commitment_tree(trees.sprout.clone(), sprout)?;
    }

    let sapling: Vec<_> = block.sapling_note_commitments().collect();
    if !sapling.is_empty() {
        let (tree, subtree) = NoteCommitmentTrees::update_sapling_note_commitment_tree(
            trees.sapling.clone(),
            sapling,
        )?;
        trees.sapling = tree;
        trees.sapling_subtree =
            subtree.map(|(index, node)| NoteCommitmentSubtree::new(index, height, node));
    }

    let orchard: Vec<_> = block.orchard_note_commitments().collect();
    if !orchard.is_empty() {
        let (tree, subtree) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            trees.orchard.clone(),
            orchard,
        )?;
        trees.orchard = tree;
        trees.orchard_subtree =
            subtree.map(|(index, node)| NoteCommitmentSubtree::new(index, height, node));
    }

    let ironwood: Vec<_> = block.ironwood_note_commitments().collect();
    if !ironwood.is_empty() {
        let (tree, subtree) = NoteCommitmentTrees::update_ironwood_note_commitment_tree(
            trees.ironwood.clone(),
            ironwood,
        )?;
        trees.ironwood = tree;
        trees.ironwood_subtree =
            subtree.map(|(index, node)| NoteCommitmentSubtree::new(index, height, node));
    }

    Ok(())
}

/// Checks that updating `parallel` with [`NoteCommitmentTrees::update_trees_parallel`] gives the
/// same trees and roots as updating `series` one pool at a time.
fn check_parallel_update_matches_series(
    parallel: &mut NoteCommitmentTrees,
    series: &mut NoteCommitmentTrees,
    block: &Arc<Block>,
    description: &str,
) {
    parallel
        .update_trees_parallel(block)
        .expect("test commitments fit in the trees");
    update_trees_in_series(series, block).expect("test commitments fit in the trees");

    assert_eq!(parallel, series, "{description}");
    assert_eq!(parallel.sprout.root(), series.sprout.root());
    assert_eq!(parallel.sapling.root(), series.sapling.root());
    assert_eq!(parallel.orchard.root(), series.orchard.root());
    assert_eq!(parallel.ironwood.root(), series.ironwood.root());
}

/// Parallel tree updates match updates of one pool at a time, whichever pool runs on the calling
/// thread.
#[test]
fn parallel_tree_updates_match_updates_in_series() {
    let _init_guard = zebra_test::init();

    let mut pool_mixes = BTreeSet::new();

    for network in Network::iter() {
        let mut parallel = NoteCommitmentTrees::default();
        let mut series = NoteCommitmentTrees::default();

        for (height, bytes) in network.block_iter() {
            let block: Arc<Block> = bytes
                .zcash_deserialize_into()
                .expect("test vector blocks are valid");

            pool_mixes.insert([
                block.sprout_note_commitments().next().is_some(),
                block.sapling_note_commitments().next().is_some(),
                block.orchard_note_commitments().next().is_some(),
            ]);

            check_parallel_update_matches_series(
                &mut parallel,
                &mut series,
                &block,
                &format!("{network} block {height}"),
            );
        }
    }

    // Each pool is updated on the calling thread when it is the costliest pool in a block, so the
    // vectors must have blocks whose costliest pool is each of Sprout, Sapling and Orchard.
    for pool in 0..3 {
        assert!(
            pool_mixes
                .iter()
                .any(|mix| mix[pool] && mix[pool + 1..].iter().all(|later| !later)),
            "the test vectors have a block whose costliest pool is pool {pool}: {pool_mixes:?}",
        );
    }
}

/// Parallel tree updates match updates of one pool at a time for generated NU6.3 blocks, including
/// blocks with Ironwood note commitments and no Orchard ones, which update Ironwood on the calling
/// thread. The test vectors have no Ironwood commitments.
#[test]
fn parallel_tree_updates_match_updates_in_series_for_ironwood_blocks() {
    let _init_guard = zebra_test::init();

    const GENERATED_BLOCKS: usize = 32;

    let strategy = LedgerState::height_strategy(Height(1), NetworkUpgrade::Nu6_3, 6, false)
        .prop_flat_map(Block::arbitrary_with);
    let mut runner = TestRunner::deterministic();

    let mut parallel = NoteCommitmentTrees::default();
    let mut series = NoteCommitmentTrees::default();
    let mut ironwood_on_calling_thread = 0;

    for index in 0..GENERATED_BLOCKS {
        let block = Arc::new(
            strategy
                .new_tree(&mut runner)
                .expect("the block strategy generates blocks")
                .current(),
        );

        if block.ironwood_note_commitments().next().is_some()
            && block.orchard_note_commitments().next().is_none()
        {
            ironwood_on_calling_thread += 1;
        }

        check_parallel_update_matches_series(
            &mut parallel,
            &mut series,
            &block,
            &format!("generated block {index}"),
        );
    }

    assert!(
        ironwood_on_calling_thread > 0,
        "some generated blocks update Ironwood on the calling thread",
    );
}

/// Returns the Orchard and Ironwood updates of `trees` for the note commitments of `block`, taken
/// one transaction at a time, as they are for transactions that aren't in a block yet.
fn precomputed_updates(
    trees: &NoteCommitmentTrees,
    block: &Block,
) -> Vec<Arc<PrecomputedTreeUpdate>> {
    let orchard: Vec<_> = block
        .transactions
        .iter()
        .flat_map(|transaction| PrecomputedTreeUpdate::orchard_note_commitments(transaction))
        .collect();
    let ironwood: Vec<_> = block
        .transactions
        .iter()
        .flat_map(|transaction| PrecomputedTreeUpdate::ironwood_note_commitments(transaction))
        .collect();

    // A block appends the note commitments of its transactions in the order it holds them.
    assert_eq!(
        orchard,
        block.orchard_note_commitments().collect::<Vec<_>>()
    );
    assert_eq!(
        ironwood,
        block.ironwood_note_commitments().collect::<Vec<_>>()
    );

    [(&trees.orchard, orchard), (&trees.ironwood, ironwood)]
        .into_iter()
        .filter(|(_tree, note_commitments)| !note_commitments.is_empty())
        .map(|(tree, note_commitments)| {
            Arc::new(
                PrecomputedTreeUpdate::new(tree.clone(), note_commitments)
                    .expect("test commitments fit in the trees"),
            )
        })
        .collect()
}

/// Tree updates that take their Orchard and Ironwood updates from precomputed ones give the same
/// trees and subtrees as updates that compute them, and they hold the precomputed trees.
#[test]
fn tree_updates_with_precomputed_updates_match_computed_updates() {
    let _init_guard = zebra_test::init();

    const GENERATED_BLOCKS: usize = 32;

    let strategy = LedgerState::height_strategy(Height(1), NetworkUpgrade::Nu6_3, 6, false)
        .prop_flat_map(Block::arbitrary_with);
    let mut runner = TestRunner::deterministic();

    let mut trees = NoteCommitmentTrees::default();
    let mut orchard_updates = 0;
    let mut ironwood_updates = 0;

    for index in 0..GENERATED_BLOCKS {
        let block = Arc::new(
            strategy
                .new_tree(&mut runner)
                .expect("the block strategy generates blocks")
                .current(),
        );
        let precomputed = precomputed_updates(&trees, &block);

        let mut computed = trees.clone();
        computed
            .update_trees_parallel(&block)
            .expect("test commitments fit in the trees");

        let has_orchard = block.orchard_note_commitments().next().is_some();
        let has_ironwood = block.ironwood_note_commitments().next().is_some();
        let has_other_pools = block.sprout_note_commitments().next().is_some()
            || block.sapling_note_commitments().next().is_some();
        assert_eq!(
            trees.has_update_work(&block, &[]),
            has_orchard || has_ironwood || has_other_pools,
        );
        assert_eq!(trees.has_update_work(&block, &precomputed), has_other_pools);

        let completed = trees
            .update_trees_parallel_with_precomputed(&block, &precomputed, || false)
            .expect("test commitments fit in the trees");
        assert!(completed);

        assert_eq!(trees, computed, "generated block {index}");
        assert_eq!(trees.orchard.root(), computed.orchard.root());
        assert_eq!(trees.ironwood.root(), computed.ironwood.root());

        let is_precomputed = |tree: &Arc<orchard::tree::NoteCommitmentTree>| {
            precomputed
                .iter()
                .any(|update| Arc::ptr_eq(update.tree(), tree))
        };
        if has_orchard {
            assert!(is_precomputed(&trees.orchard), "generated block {index}");
            orchard_updates += 1;
        }
        if has_ironwood {
            assert!(is_precomputed(&trees.ironwood), "generated block {index}");
            ironwood_updates += 1;
        }
    }

    assert!(
        orchard_updates > 0 && ironwood_updates > 0,
        "some generated blocks have Orchard note commitments, and some have Ironwood ones",
    );
}

/// A precomputed update is only found for the tree and the note commitments it was computed for,
/// and it is what the update of that tree computes.
#[test]
fn precomputed_update_is_only_found_for_its_tree_and_note_commitments() {
    let _init_guard = zebra_test::init();

    let note_commitments: Vec<_> = (1..=4_u64).map(pallas::Base::from).collect();
    let mut parent = orchard::tree::NoteCommitmentTree::default();
    parent
        .append(pallas::Base::from(100))
        .expect("an empty tree has room");
    let parent = Arc::new(parent);

    let update = Arc::new(
        PrecomputedTreeUpdate::new(parent.clone(), note_commitments.clone())
            .expect("the tree has room"),
    );
    let precomputed = [update.clone()];
    let find = |tree: &Arc<orchard::tree::NoteCommitmentTree>,
                note_commitments: &[pallas::Base]| {
        PrecomputedTreeUpdate::find(&precomputed, tree, note_commitments)
    };

    let (computed, computed_subtree) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
        parent.clone(),
        note_commitments.clone(),
    )
    .expect("the tree has room");
    let (found, found_subtree) = find(&parent, &note_commitments).expect("the update is found");
    assert!(Arc::ptr_eq(&found, update.tree()));
    assert!(found.cached_root().is_some());
    assert_eq!(found.root(), computed.root());
    assert_eq!(found_subtree, computed_subtree);

    // Another copy of the same tree, whether or not either copy has its root.
    let copy = Arc::new((*parent).clone());
    assert!(find(&copy, &note_commitments).is_some());
    let _ = copy.root();
    assert!(find(&copy, &note_commitments).is_some());

    // The same note commitments in another order, some of them, and more of them.
    let mut reordered = note_commitments.clone();
    reordered.swap(0, 1);
    assert!(find(&parent, &reordered).is_none());
    assert!(find(&parent, &note_commitments[..3]).is_none());
    let mut extended = note_commitments.clone();
    extended.push(pallas::Base::from(5));
    assert!(find(&parent, &extended).is_none());
    assert!(find(&parent, &[]).is_none());

    // Other trees.
    let mut longer = (*parent).clone();
    longer
        .append(pallas::Base::from(101))
        .expect("the tree has room");
    assert!(find(&Arc::new(longer), &note_commitments).is_none());
    assert!(find(&Arc::default(), &note_commitments).is_none());
}
