//! Fully populate the Sapling and Orchard note commitment subtrees for existing blocks in the database.

use std::sync::{mpsc, Arc};

use zebra_chain::{
    block::Height,
    orchard, sapling,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
};

use crate::service::finalized_state::{
    disk_format::upgrade::CancelFormatChange, DiskWriteBatch, ZebraDb,
};

/// Runs disk format upgrade for adding Sapling and Orchard note commitment subtrees to database.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
pub fn run(
    initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.sapling_tree_by_height_range(..=initial_tip_height) {
        // Return early if there is a cancel signal.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Empty note commitment trees can't contain subtrees.
        let Some(end_of_block_subtree_index) = tree.subtree_index() else {
            prev_tree = Some(tree);
            continue;
        };

        // Blocks cannot complete multiple level 16 subtrees,
        // so the subtree index can increase by a maximum of 1 every ~20 blocks.
        // If this block does complete a subtree, the subtree is either completed by a note before
        // the final note (so the final note is in the next subtree), or by the final note
        // (so the final note is the end of this subtree).

        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            // If the leaf at the end of the block is the final leaf in a subtree,
            // we already have that subtree root available in the tree.
            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_sapling_subtree(upgrade_db, index, height, node);
            subtree_count += 1;
        } else if end_of_block_subtree_index.0 > subtree_count {
            // If the leaf at the end of the block is in the next subtree,
            // we need to calculate that subtree root based on the tree from the previous block.
            let mut prev_tree = prev_tree
                .take()
                .expect("should have some previous sapling frontier");
            let sapling_nct = Arc::make_mut(&mut prev_tree);

            let block = upgrade_db
                .block(height.into())
                .expect("height with note commitment tree should have block");

            for sapling_note_commitment in block.sapling_note_commitments() {
                // Return early if there is a cancel signal.
                if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                sapling_nct
                    .append(*sapling_note_commitment)
                    .expect("finalized notes should append successfully");

                // The loop always breaks on this condition,
                // because we checked the block has enough commitments,
                // and that the final commitment in the block doesn't complete a subtree.
                if sapling_nct.is_complete_subtree() {
                    break;
                }
            }

            let (index, node) = sapling_nct.completed_subtree_index_and_root().expect(
                "block should have completed a subtree before its final note commitment: \
                 already checked is_complete_subtree(), and that the block must complete a subtree",
            );

            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_sapling_subtree(upgrade_db, index, height, node);
            subtree_count += 1;
        }

        prev_tree = Some(tree);
    }

    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.orchard_tree_by_height_range(..=initial_tip_height) {
        // Return early if there is a cancel signal.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Empty note commitment trees can't contain subtrees.
        let Some(end_of_block_subtree_index) = tree.subtree_index() else {
            prev_tree = Some(tree);
            continue;
        };

        // Blocks cannot complete multiple level 16 subtrees,
        // so the subtree index can increase by a maximum of 1 every ~20 blocks.
        // If this block does complete a subtree, the subtree is either completed by a note before
        // the final note (so the final note is in the next subtree), or by the final note
        // (so the final note is the end of this subtree).

        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            // If the leaf at the end of the block is the final leaf in a subtree,
            // we already have that subtree root available in the tree.
            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_orchard_subtree(upgrade_db, index, height, node);
            subtree_count += 1;
        } else if end_of_block_subtree_index.0 > subtree_count {
            // If the leaf at the end of the block is in the next subtree,
            // we need to calculate that subtree root based on the tree from the previous block.
            let mut prev_tree = prev_tree
                .take()
                .expect("should have some previous orchard frontier");
            let orchard_nct = Arc::make_mut(&mut prev_tree);

            let block = upgrade_db
                .block(height.into())
                .expect("height with note commitment tree should have block");

            for orchard_note_commitment in block.orchard_note_commitments() {
                // Return early if there is a cancel signal.
                if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                orchard_nct
                    .append(*orchard_note_commitment)
                    .expect("finalized notes should append successfully");

                // The loop always breaks on this condition,
                // because we checked the block has enough commitments,
                // and that the final commitment in the block doesn't complete a subtree.
                if orchard_nct.is_complete_subtree() {
                    break;
                }
            }

            let (index, node) = orchard_nct.completed_subtree_index_and_root().expect(
                "block should have completed a subtree before its final note commitment: \
                 already checked is_complete_subtree(), and that the block must complete a subtree",
            );

            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_orchard_subtree(upgrade_db, index, height, node);
            subtree_count += 1;
        }

        prev_tree = Some(tree);
    }

    Ok(())
}

/// Check that note commitment subtrees were correctly added.
///
/// # Panics
///
/// If a note commitment subtree is missing or incorrect.
pub fn check(db: &ZebraDb) {
    let check_sapling_subtrees = check_sapling_subtrees(db);
    let check_orchard_subtrees = check_orchard_subtrees(db);
    if !check_sapling_subtrees || !check_orchard_subtrees {
        panic!("missing or bad subtree(s)");
    }
}

/// Check that Sapling note commitment subtrees were correctly added.
///
/// # Panics
///
/// If a note commitment subtree is missing or incorrect.
fn check_sapling_subtrees(db: &ZebraDb) -> bool {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.sapling_tree().subtree_index()
    else {
        return true;
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.sapling_tree().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut is_valid = true;
    for index in 0..first_incomplete_subtree_index {
        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            error!(index, "missing subtree");
            is_valid = false;
            continue;
        };

        // Check that there was a sapling note at the subtree's end height.
        let Some(tree) = db.sapling_tree_by_height(&subtree.end) else {
            error!(?subtree.end, "missing note commitment tree at subtree completion height");
            is_valid = false;
            continue;
        };

        // Check the index and root if the sapling note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                error!("completed subtree indexes should match");
                is_valid = false;
            }

            if subtree.node != node {
                error!("completed subtree roots should match");
                is_valid = false;
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let Some(prev_tree) = db.sapling_tree_by_height(&subtree.end.previous()) else {
                error!(?subtree.end, "missing note commitment tree at subtree completion height");
                is_valid = false;
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                error!(
                    ?subtree_index,
                    ?prev_subtree_index,
                    "note commitment tree at end height should have incremented subtree index"
                );
                is_valid = false;
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .sapling_tree_by_height_range(..)
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
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
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            error!(?index, "missing subtree");
            is_valid = false;
            continue;
        };

        if subtree.index.0 != index {
            error!("completed subtree indexes should match");
            is_valid = false;
        }

        if subtree.end != height {
            let is_complete = tree.is_complete_subtree();
            error!(?subtree.end, ?height, ?index, ?is_complete, "bad sapling subtree end height");
            is_valid = false;
        }

        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.node != node {
                error!("completed subtree roots should match");
                is_valid = false;
            }
        }
    }

    if !is_valid {
        error!(
            ?subtree_count,
            first_incomplete_subtree_index, "missing or bad sapling subtrees"
        );
    }

    is_valid
}

/// Check that Orchard note commitment subtrees were correctly added.
///
/// # Panics
///
/// If a note commitment subtree is missing or incorrect.
fn check_orchard_subtrees(db: &ZebraDb) -> bool {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.orchard_tree().subtree_index()
    else {
        return true;
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.orchard_tree().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut is_valid = true;
    for index in 0..first_incomplete_subtree_index {
        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            error!(index, "missing subtree");
            is_valid = false;
            continue;
        };

        // Check that there was a orchard note at the subtree's end height.
        let Some(tree) = db.orchard_tree_by_height(&subtree.end) else {
            error!(?subtree.end, "missing note commitment tree at subtree completion height");
            is_valid = false;
            continue;
        };

        // Check the index and root if the orchard note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                error!("completed subtree indexes should match");
                is_valid = false;
            }

            if subtree.node != node {
                error!("completed subtree roots should match");
                is_valid = false;
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let Some(prev_tree) = db.orchard_tree_by_height(&subtree.end.previous()) else {
                error!(?subtree.end, "missing note commitment tree at subtree completion height");
                is_valid = false;
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                error!(
                    ?subtree_index,
                    ?prev_subtree_index,
                    "note commitment tree at end height should have incremented subtree index"
                );
                is_valid = false;
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .orchard_tree_by_height_range(..)
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
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
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            error!(?index, "missing subtree");
            is_valid = false;
            continue;
        };

        if subtree.index.0 != index {
            error!("completed subtree indexes should match");
            is_valid = false;
        }

        if subtree.end != height {
            let is_complete = tree.is_complete_subtree();
            error!(?subtree.end, ?height, ?index, ?is_complete, "bad orchard subtree end height");
            is_valid = false;
        }

        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.node != node {
                error!("completed subtree roots should match");
                is_valid = false;
            }
        }
    }

    if !is_valid {
        error!(
            ?subtree_count,
            first_incomplete_subtree_index, "missing or bad orchard subtrees"
        );
    }

    is_valid
}

/// Writes a Sapling note commitment subtree to `upgrade_db`.
fn write_sapling_subtree(
    upgrade_db: &ZebraDb,
    index: NoteCommitmentSubtreeIndex,
    height: Height,
    node: sapling::tree::Node,
) {
    let subtree = NoteCommitmentSubtree::new(index, height, node);

    let mut batch = DiskWriteBatch::new();

    batch.insert_sapling_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing sapling note commitment subtrees should always succeed.");

    if index.0 % 100 == 0 {
        info!(?height, index = ?index.0, "calculated and added sapling subtree");
    }
    // This log happens about once per second on recent machines with SSD disks.
    debug!(?height, index = ?index.0, ?node, "calculated and added sapling subtree");
}

/// Writes a Orchard note commitment subtree to `upgrade_db`.
fn write_orchard_subtree(
    upgrade_db: &ZebraDb,
    index: NoteCommitmentSubtreeIndex,
    height: Height,
    node: orchard::tree::Node,
) {
    let subtree = NoteCommitmentSubtree::new(index, height, node);

    let mut batch = DiskWriteBatch::new();

    batch.insert_orchard_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing orchard note commitment subtrees should always succeed.");

    if index.0 % 300 == 0 {
        info!(?height, index = ?index.0, "calculated and added orchard subtree");
    }
    // This log happens about 3 times per second on recent machines with SSD disks.
    debug!(?height, index = ?index.0, ?node, "calculated and added orchard subtree");
}
