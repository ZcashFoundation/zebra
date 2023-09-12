//! Fully populate the Sapling and Orchard note commitment subtrees for existing blocks in the database.

use std::sync::{mpsc, Arc};

use hex_literal::hex;
use itertools::Itertools;

use zebra_chain::{
    block::Height,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::Network::*,
    sapling,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
};

use crate::service::finalized_state::{
    disk_format::upgrade::CancelFormatChange, DiskWriteBatch, ZebraDb,
};

/// Runs disk format upgrade for adding Sapling and Orchard note commitment subtrees to database.
///
/// Trees are added to the database in reverse height order, so that wallets can sync correctly
/// while the upgrade is running.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
pub fn run(
    initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    // # Consensus
    //
    // Zebra stores exactly one note commitment tree for every block with sapling notes.
    // (It also stores the empty note commitment tree for the genesis block, but we skip that.)
    //
    // The consensus rules limit blocks to less than 2^16 sapling and 2^16 orchard outputs. So a
    // block can't complete multiple level 16 subtrees (or complete an entire subtree by itself).
    // Currently, with 2MB blocks and v4/v5 sapling and orchard output sizes, the subtree index can
    // increase by 1 every ~20 full blocks.
    //
    // Therefore, the first block with shielded note can't complete a subtree, which means we can
    // skip the (genesis block, first shielded block) tree pair.
    //
    // # Compatibility
    //
    // Because wallets search backwards from the chain tip, subtrees need to be added to the
    // database in reverse height order. (Tip first, genesis last.)
    //
    // Otherwise, wallets that sync during the upgrade will be missing some notes.

    // Generate a list of sapling subtree inputs: previous tree, current tree, and end height.
    let subtrees = upgrade_db
        .sapling_tree_by_reversed_height_range(..=initial_tip_height)
        // The first block with sapling notes can't complete a subtree, see above for details.
        .filter(|(height, _tree)| !height.is_min())
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        // Because the iterator is reversed, the larger tree is first.
        .map(|((end_height, tree), (_prev_height, prev_tree))| (prev_tree, end_height, tree))
        // Empty note commitment trees can't contain subtrees, so they have invalid subtree indexes.
        // But since we skip the empty genesis tree, all trees must have valid indexes.
        // So we don't need to unwrap the optional values for this comparison to be correct.
        .filter(|(prev_tree, _end_height, tree)| tree.subtree_index() > prev_tree.subtree_index());

    for (prev_tree, end_height, tree) in subtrees {
        // Return early if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        let subtree = calculate_sapling_subtree(upgrade_db, prev_tree, end_height, tree);
        write_sapling_subtree(upgrade_db, subtree);
    }

    // Generate a list of orchard subtree inputs: previous tree, current tree, and end height.
    let subtrees = upgrade_db
        .orchard_tree_by_reversed_height_range(..=initial_tip_height)
        // The first block with orchard notes can't complete a subtree, see above for details.
        .filter(|(height, _tree)| !height.is_min())
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        // Because the iterator is reversed, the larger tree is first.
        .map(|((end_height, tree), (_prev_height, prev_tree))| (prev_tree, end_height, tree))
        // Empty note commitment trees can't contain subtrees, so they have invalid subtree indexes.
        // But since we skip the empty genesis tree, all trees must have valid indexes.
        // So we don't need to unwrap the optional values for this comparison to be correct.
        .filter(|(prev_tree, _end_height, tree)| tree.subtree_index() > prev_tree.subtree_index());

    for (prev_tree, end_height, tree) in subtrees {
        // Return early if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        let subtree = calculate_orchard_subtree(upgrade_db, prev_tree, end_height, tree);
        write_orchard_subtree(upgrade_db, subtree);
    }

    Ok(())
}

/// Reset data from previous upgrades. This data can be complete or incomplete.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
pub fn reset(
    _initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    // Return early if the upgrade is cancelled.
    if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    // This doesn't delete the maximum index, but the consensus rules make that subtree impossible.
    // (Adding a note to a full note commitment tree is an error.)
    //
    // TODO: convert zs_delete_range() to take std::ops::RangeBounds, and delete the upper bound.
    let mut batch = DiskWriteBatch::new();
    batch.delete_range_sapling_subtree(upgrade_db, 0.into(), u16::MAX.into());
    upgrade_db
        .write_batch(batch)
        .expect("deleting old sapling note commitment subtrees is a valid database operation");

    if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    let mut batch = DiskWriteBatch::new();
    batch.delete_range_orchard_subtree(upgrade_db, 0.into(), u16::MAX.into());
    upgrade_db
        .write_batch(batch)
        .expect("deleting old orchard note commitment subtrees is a valid database operation");

    Ok(())
}

/// Quickly check that the first calculated subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// # Panics
///
/// If a note commitment subtree is missing or incorrect.
pub fn quick_check(db: &ZebraDb) {
    let sapling_result = quick_check_sapling_subtrees(db);
    let orchard_result = quick_check_orchard_subtrees(db);

    if sapling_result.is_err() || orchard_result.is_err() {
        // TODO: when the check functions are refactored so they are called from a single function,
        //       move this panic into that function, but still log a detailed message here
        panic!(
            "missing or bad first subtree: sapling: {sapling_result:?}, orchard: {orchard_result:?}"
        );
    }
}

/// A quick test vector that allows us to fail an incorrect upgrade within a few seconds.
fn first_sapling_mainnet_subtree() -> NoteCommitmentSubtree<sapling::tree::Node> {
    // This test vector was generated using the command:
    // ```sh
    // zcash-cli z_getsubtreesbyindex sapling 0 1
    // ```
    NoteCommitmentSubtree {
        index: 0.into(),
        node: hex!("754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13")
            .as_slice()
            .try_into()
            .expect("test vector is valid"),
        end: Height(558822),
    }
}

/// A quick test vector that allows us to fail an incorrect upgrade within a few seconds.
fn first_orchard_mainnet_subtree() -> NoteCommitmentSubtree<orchard::tree::Node> {
    // This test vector was generated using the command:
    // ```sh
    // zcash-cli z_getsubtreesbyindex orchard 0 1
    // ```
    NoteCommitmentSubtree {
        index: 0.into(),
        node: hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
            .as_slice()
            .try_into()
            .expect("test vector is valid"),
        end: Height(1707429),
    }
}

/// Quickly check that the first calculated sapling subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn quick_check_sapling_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    // We check the first sapling tree on mainnet, so skip this check if it isn't available.
    let Some(NoteCommitmentSubtreeIndex(first_incomplete_subtree_index)) =
        db.sapling_tree().subtree_index()
    else {
        return Ok(());
    };

    if first_incomplete_subtree_index == 0 || db.network() != Mainnet {
        return Ok(());
    }

    // Find the first complete subtree, with its note commitment tree, end height, and the previous tree.
    let first_complete_subtree = db
        .sapling_tree_by_height_range(..)
        // The first block with sapling notes can't complete a subtree, see above for details.
        .filter(|(height, _tree)| !height.is_min())
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        .map(|((_prev_height, prev_tree), (end_height, tree))| (prev_tree, end_height, tree))
        // Empty note commitment trees can't contain subtrees, so they have invalid subtree indexes.
        // But since we skip the empty genesis tree, all trees must have valid indexes.
        // So we don't need to unwrap the optional values for this comparison to be correct.
        .find(|(prev_tree, _end_height, tree)| tree.subtree_index() > prev_tree.subtree_index());

    let Some((prev_tree, end_height, tree)) = first_complete_subtree else {
        let result = Err("iterator did not find complete subtree, but the tree has it");
        error!(?result);
        return result;
    };

    // Creating this test vector involves a cryptographic check, so only do it once.
    let expected_subtree = first_sapling_mainnet_subtree();

    let db_subtree = calculate_sapling_subtree(db, prev_tree, end_height, tree);

    if db_subtree != expected_subtree {
        let result = Err("first subtree did not match expected test vector");
        error!(?result, ?db_subtree, ?expected_subtree);
        return result;
    }

    Ok(())
}

/// Quickly check that the first calculated orchard subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn quick_check_orchard_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    // We check the first orchard tree on mainnet, so skip this check if it isn't available.
    let Some(NoteCommitmentSubtreeIndex(first_incomplete_subtree_index)) =
        db.orchard_tree().subtree_index()
    else {
        return Ok(());
    };

    if first_incomplete_subtree_index == 0 || db.network() != Mainnet {
        return Ok(());
    }

    // Find the first complete subtree, with its note commitment tree, end height, and the previous tree.
    let first_complete_subtree = db
        .orchard_tree_by_height_range(..)
        // The first block with orchard notes can't complete a subtree, see above for details.
        .filter(|(height, _tree)| !height.is_min())
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        .map(|((_prev_height, prev_tree), (end_height, tree))| (prev_tree, end_height, tree))
        // Empty note commitment trees can't contain subtrees, so they have invalid subtree indexes.
        // But since we skip the empty genesis tree, all trees must have valid indexes.
        // So we don't need to unwrap the optional values for this comparison to be correct.
        .find(|(prev_tree, _end_height, tree)| tree.subtree_index() > prev_tree.subtree_index());

    let Some((prev_tree, end_height, tree)) = first_complete_subtree else {
        let result = Err("iterator did not find complete subtree, but the tree has it");
        error!(?result);
        return result;
    };

    // Creating this test vector involves a cryptographic check, so only do it once.
    let expected_subtree = first_orchard_mainnet_subtree();

    let db_subtree = calculate_orchard_subtree(db, prev_tree, end_height, tree);

    if db_subtree != expected_subtree {
        let result = Err("first subtree did not match expected test vector");
        error!(?result, ?db_subtree, ?expected_subtree);
        return result;
    }

    Ok(())
}

/// Check that note commitment subtrees were correctly added.
///
/// # Panics
///
/// If a note commitment subtree is missing or incorrect.
pub fn check(db: &ZebraDb) {
    let sapling_result = check_sapling_subtrees(db);
    let orchard_result = check_orchard_subtrees(db);

    if sapling_result.is_err() || orchard_result.is_err() {
        // TODO: when the check functions are refactored so they are called from a single function,
        //       move this panic into that function, but still log a detailed message here
        panic!(
            "missing or bad subtree(s): sapling: {sapling_result:?}, orchard: {orchard_result:?}"
        );
    }
}

/// Check that Sapling note commitment subtrees were correctly added.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn check_sapling_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.sapling_tree().subtree_index()
    else {
        return Ok(());
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.sapling_tree().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut result = Ok(());
    for index in 0..first_incomplete_subtree_index {
        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that there was a sapling note at the subtree's end height.
        let Some(tree) = db.sapling_tree_by_height(&subtree.end) else {
            result = Err("missing note commitment tree at subtree completion height");
            error!(?result, ?subtree.end);
            continue;
        };

        // Check the index and root if the sapling note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                result = Err("completed subtree indexes should match");
                error!(?result);
            }

            if subtree.node != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let Some(prev_tree) = db.sapling_tree_by_height(&subtree.end.previous()) else {
                result = Err("missing note commitment tree at subtree completion height");
                error!(?result, ?subtree.end);
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                result =
                    Err("note commitment tree at end height should have incremented subtree index");
                error!(?result, ?subtree_index, ?prev_subtree_index,);
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .sapling_tree_by_height_range(..)
        // Exclude empty sapling tree and add subtree indexes
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
        // Exclude heights that don't complete a subtree and count completed subtrees
        .filter_map(|(subtree_index, height, tree)| {
            if tree.is_complete_subtree() || subtree_index.0 > subtree_count {
                let subtree_index = subtree_count;
                subtree_count += 1;
                Some((subtree_index, height, tree))
            } else {
                None
            }
        })
    {
        // Check that there's an entry for every completed sapling subtree root in all sapling trees
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that the subtree end height matches that in the sapling trees.
        if subtree.end != height {
            let is_complete = tree.is_complete_subtree();
            result = Err("bad sapling subtree end height");
            error!(?result, ?subtree.end, ?height, ?index, ?is_complete, );
        }

        // Check the root if the sapling note commitment tree at this height is a complete subtree.
        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.node != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
    }

    if result.is_err() {
        error!(
            ?result,
            ?subtree_count,
            first_incomplete_subtree_index,
            "missing or bad sapling subtrees"
        );
    }

    result
}

/// Check that Orchard note commitment subtrees were correctly added.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn check_orchard_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.orchard_tree().subtree_index()
    else {
        return Ok(());
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.orchard_tree().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut result = Ok(());
    for index in 0..first_incomplete_subtree_index {
        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that there was a orchard note at the subtree's end height.
        let Some(tree) = db.orchard_tree_by_height(&subtree.end) else {
            result = Err("missing note commitment tree at subtree completion height");
            error!(?result, ?subtree.end);
            continue;
        };

        // Check the index and root if the orchard note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                result = Err("completed subtree indexes should match");
                error!(?result);
            }

            if subtree.node != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let Some(prev_tree) = db.orchard_tree_by_height(&subtree.end.previous()) else {
                result = Err("missing note commitment tree at subtree completion height");
                error!(?result, ?subtree.end);
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                result =
                    Err("note commitment tree at end height should have incremented subtree index");
                error!(?result, ?subtree_index, ?prev_subtree_index,);
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .orchard_tree_by_height_range(..)
        // Exclude empty orchard tree and add subtree indexes
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
        // Exclude heights that don't complete a subtree and count completed subtrees
        .filter_map(|(subtree_index, height, tree)| {
            if tree.is_complete_subtree() || subtree_index.0 > subtree_count {
                let subtree_index = subtree_count;
                subtree_count += 1;
                Some((subtree_index, height, tree))
            } else {
                None
            }
        })
    {
        // Check that there's an entry for every completed orchard subtree root in all orchard trees
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that the subtree end height matches that in the orchard trees.
        if subtree.end != height {
            let is_complete = tree.is_complete_subtree();
            result = Err("bad orchard subtree end height");
            error!(?result, ?subtree.end, ?height, ?index, ?is_complete, );
        }

        // Check the root if the orchard note commitment tree at this height is a complete subtree.
        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.node != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
    }

    if result.is_err() {
        error!(
            ?result,
            ?subtree_count,
            first_incomplete_subtree_index,
            "missing or bad orchard subtrees"
        );
    }

    result
}

/// Calculates a Sapling note commitment subtree, reading blocks from `read_db` if needed.
///
/// `tree` must be a note commitment tree containing a recently completed subtree,
/// which was not already completed in `prev_tree`.
///
/// `prev_tree` is only used to rebuild the subtree, if it was completed inside the block.
/// If the subtree was completed by the final note commitment in the block, `prev_tree` is unused.
///
/// # Panics
///
/// If `tree` does not contain a recently completed subtree.
#[must_use = "subtree should be written to the database after it is calculated"]
fn calculate_sapling_subtree(
    read_db: &ZebraDb,
    prev_tree: Arc<sapling::tree::NoteCommitmentTree>,
    end_height: Height,
    tree: Arc<sapling::tree::NoteCommitmentTree>,
) -> NoteCommitmentSubtree<sapling::tree::Node> {
    // If this block completed a subtree, the subtree is either completed by a note before
    // the final note (so the final note is in the next subtree), or by the final note
    // (so the final note is the end of this subtree).
    if let Some((index, node)) = tree.completed_subtree_index_and_root() {
        // If the leaf at the end of the block is the final leaf in a subtree,
        // we already have that subtree root available in the tree.
        NoteCommitmentSubtree::new(index, end_height, node)
    } else {
        // If the leaf at the end of the block is in the next subtree,
        // we need to calculate that subtree root based on the tree from the previous block.
        let index = prev_tree
            .subtree_index()
            .expect("previous block must have a partial subtree");
        let remaining_notes = prev_tree.remaining_subtree_leaf_nodes();
        let is_complete = prev_tree.is_complete_subtree();

        assert_eq!(
            index.0 + 1,
            tree.subtree_index()
                .expect("current block must have a subtree")
                .0,
            "tree must have been completed by the current block"
        );
        assert!(remaining_notes > 0, "just checked for a complete tree");
        assert!(!is_complete, "just checked for a complete tree");

        let block = read_db
            .block(end_height.into())
            .expect("height with note commitment tree should have block");
        let sapling_note_commitments = block
            .sapling_note_commitments()
            .take(remaining_notes)
            .cloned()
            .collect();

        // This takes less than 1 second per tree, so we don't need to make it cancellable.
        let (sapling_nct, subtree) = NoteCommitmentTrees::update_sapling_note_commitment_tree(
            prev_tree,
            sapling_note_commitments,
        )
        .expect("finalized notes should append successfully");

        let (index, node) = subtree.unwrap_or_else(|| {
            panic!(
                "already checked that the block completed a subtree:\n\
                 updated subtree:\n\
                 index: {:?}\n\
                 remaining notes: {}\n\
                 is complete: {}\n\
                 original subtree:\n\
                 index: {index:?}\n\
                 remaining notes: {remaining_notes}\n\
                 is complete: {is_complete}\n",
                sapling_nct.subtree_index(),
                sapling_nct.remaining_subtree_leaf_nodes(),
                sapling_nct.is_complete_subtree(),
            )
        });

        NoteCommitmentSubtree::new(index, end_height, node)
    }
}

/// Calculates a Orchard note commitment subtree, reading blocks from `read_db` if needed.
///
/// `tree` must be a note commitment tree containing a recently completed subtree,
/// which was not already completed in `prev_tree`.
///
/// `prev_tree` is only used to rebuild the subtree, if it was completed inside the block.
/// If the subtree was completed by the final note commitment in the block, `prev_tree` is unused.
///
/// # Panics
///
/// If `tree` does not contain a recently completed subtree.
#[must_use = "subtree should be written to the database after it is calculated"]
fn calculate_orchard_subtree(
    read_db: &ZebraDb,
    prev_tree: Arc<orchard::tree::NoteCommitmentTree>,
    end_height: Height,
    tree: Arc<orchard::tree::NoteCommitmentTree>,
) -> NoteCommitmentSubtree<orchard::tree::Node> {
    // If this block completed a subtree, the subtree is either completed by a note before
    // the final note (so the final note is in the next subtree), or by the final note
    // (so the final note is the end of this subtree).
    if let Some((index, node)) = tree.completed_subtree_index_and_root() {
        // If the leaf at the end of the block is the final leaf in a subtree,
        // we already have that subtree root available in the tree.
        NoteCommitmentSubtree::new(index, end_height, node)
    } else {
        // If the leaf at the end of the block is in the next subtree,
        // we need to calculate that subtree root based on the tree from the previous block.
        let index = prev_tree
            .subtree_index()
            .expect("previous block must have a partial subtree");
        let remaining_notes = prev_tree.remaining_subtree_leaf_nodes();
        let is_complete = prev_tree.is_complete_subtree();

        assert_eq!(
            index.0 + 1,
            tree.subtree_index()
                .expect("current block must have a subtree")
                .0,
            "tree must have been completed by the current block"
        );
        assert!(remaining_notes > 0, "just checked for a complete tree");
        assert!(!is_complete, "just checked for a complete tree");

        let block = read_db
            .block(end_height.into())
            .expect("height with note commitment tree should have block");
        let orchard_note_commitments = block
            .orchard_note_commitments()
            .take(remaining_notes)
            .cloned()
            .collect();

        // This takes less than 1 second per tree, so we don't need to make it cancellable.
        let (orchard_nct, subtree) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            prev_tree,
            orchard_note_commitments,
        )
        .expect("finalized notes should append successfully");

        let (index, node) = subtree.unwrap_or_else(|| {
            panic!(
                "already checked that the block completed a subtree:\n\
                 updated subtree:\n\
                 index: {:?}\n\
                 remaining notes: {}\n\
                 is complete: {}\n\
                 original subtree:\n\
                 index: {index:?}\n\
                 remaining notes: {remaining_notes}\n\
                 is complete: {is_complete}\n",
                orchard_nct.subtree_index(),
                orchard_nct.remaining_subtree_leaf_nodes(),
                orchard_nct.is_complete_subtree(),
            )
        });

        NoteCommitmentSubtree::new(index, end_height, node)
    }
}

/// Writes a Sapling note commitment subtree to `upgrade_db`.
fn write_sapling_subtree(
    upgrade_db: &ZebraDb,
    subtree: NoteCommitmentSubtree<sapling::tree::Node>,
) {
    let mut batch = DiskWriteBatch::new();

    batch.insert_sapling_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing sapling note commitment subtrees should always succeed.");

    if subtree.index.0 % 100 == 0 {
        info!(end_height = ?subtree.end, index = ?subtree.index.0, "calculated and added sapling subtree");
    }
    // This log happens about once per second on recent machines with SSD disks.
    debug!(end_height = ?subtree.end, index = ?subtree.index.0, "calculated and added sapling subtree");
}

/// Writes a Orchard note commitment subtree to `upgrade_db`.
fn write_orchard_subtree(
    upgrade_db: &ZebraDb,
    subtree: NoteCommitmentSubtree<orchard::tree::Node>,
) {
    let mut batch = DiskWriteBatch::new();

    batch.insert_orchard_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing orchard note commitment subtrees should always succeed.");

    if subtree.index.0 % 100 == 0 {
        info!(end_height = ?subtree.end, index = ?subtree.index.0, "calculated and added orchard subtree");
    }
    // This log happens about once per second on recent machines with SSD disks.
    debug!(end_height = ?subtree.end, index = ?subtree.index.0, "calculated and added orchard subtree");
}
