//! Tests for parallel note commitment tree updates.

use std::{collections::BTreeSet, sync::Arc};

use proptest::{
    arbitrary::Arbitrary,
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};

use crate::{
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
    LedgerState,
};

use super::{NoteCommitmentTreeError, NoteCommitmentTrees};

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
