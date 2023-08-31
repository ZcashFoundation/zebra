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
pub fn run(
    initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: mpsc::Receiver<CancelFormatChange>,
) {
    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.sapling_tree_by_height_range(..=initial_tip_height) {
        // Return early if there is a cancel signal.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return;
        }

        // Empty note commitment trees can't contain subtrees.
        let Some(end_of_block_subtree_index) = tree.subtree_index() else {
            prev_tree = Some(tree);
            continue;
        };

        // Blocks cannot complete multiple level 16 subtrees,
        // the subtree index can increase by a maximum of 1 every ~20 blocks.
        // If this block does complete a subtree, the subtree is either completed by a note before
        // the final note (so the final note is in the next subtree), or by the final note
        // (so the final note is the end of this subtree).
        if end_of_block_subtree_index.0 <= subtree_count && !tree.is_complete_subtree() {
            prev_tree = Some(tree);
            continue;
        }

        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_sapling_subtree(upgrade_db, index, height, node);
        } else {
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
                    return;
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
                 already checked is_complete_subtree(),\
                 and that the block must complete a subtree",
            );

            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_sapling_subtree(upgrade_db, index, height, node);
        };

        subtree_count += 1;
        prev_tree = Some(tree);
    }

    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.orchard_tree_by_height_range(..=initial_tip_height) {
        // Return early if there is a cancel signal.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return;
        }

        // Empty note commitment trees can't contain subtrees.
        let Some(end_of_block_subtree_index) = tree.subtree_index() else {
            prev_tree = Some(tree);
            continue;
        };

        // Blocks cannot complete multiple level 16 subtrees.
        // If a block does complete a subtree, it is either inside the block, or at the end.
        // (See the detailed comment for Sapling.)
        if end_of_block_subtree_index.0 <= subtree_count && !tree.is_complete_subtree() {
            prev_tree = Some(tree);
            continue;
        }

        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_orchard_subtree(upgrade_db, index, height, node);
        } else {
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
                    return;
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
                 already checked is_complete_subtree(),\
                 and that the block must complete a subtree",
            );

            assert_eq!(
                index.0, subtree_count,
                "trees are inserted in order with no gaps"
            );
            write_orchard_subtree(upgrade_db, index, height, node);
        };

        subtree_count += 1;
        prev_tree = Some(tree);
    }
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

    batch.insert_sapling_subtree(upgrade_db, subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing sapling note commitment subtrees should always succeed.");
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

    batch.insert_orchard_subtree(upgrade_db, subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing orchard note commitment subtrees should always succeed.");
}
