//! Fully populate the Sapling and Orchard note commitment subtrees for existing blocks in the database.

use std::sync::{
    atomic::{self, AtomicBool},
    Arc,
};

use zebra_chain::{
    block::Height, orchard::tree::NoteCommitmentTree as OrchardNoteCommitmentTree,
    sapling::tree::NoteCommitmentTree as SaplingNoteCommitmentTree, subtree::NoteCommitmentSubtree,
};

use crate::service::finalized_state::{DiskWriteBatch, ZebraDb};

/// Runs disk format upgrade for adding Sapling and Orchard note commitment subtrees to database.
pub fn _run(
    initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    should_cancel_format_change: Arc<AtomicBool>,
) {
    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.sapling_tree_by_height_range(..initial_tip_height) {
        if should_cancel_format_change.load(atomic::Ordering::Relaxed) {
            break;
        }

        let Some(frontier) = tree.frontier() else {
            prev_tree = Some(tree);
            continue
        };

        // Blocks cannot complete multiple level 16 subtrees,
        // the subtree index can increase by a maximum of 1 every ~20 blocks.
        let subtree_address = SaplingNoteCommitmentTree::subtree_address(frontier);
        if subtree_address.index() <= subtree_count {
            prev_tree = Some(tree);
            continue;
        }

        let (index, node) = if SaplingNoteCommitmentTree::is_complete_subtree(frontier) {
            tree.completed_subtree_index_and_root()
                .expect("already checked is_complete_subtree()")
        } else {
            let mut sapling_nct = Arc::try_unwrap(
                prev_tree
                    .take()
                    .expect("should have some previous sapling frontier"),
            )
            .unwrap_or_else(|shared_tree| (*shared_tree).clone());

            let block = upgrade_db
                .block(height.into())
                .expect("height with note commitment tree should have block");

            let sapling_note_commitments: Vec<_> = block
                .transactions
                .iter()
                .flat_map(|tx| tx.sapling_note_commitments())
                .cloned()
                .collect();

            for sapling_note_commitment in sapling_note_commitments {
                sapling_nct
                    .append(sapling_note_commitment)
                    .expect("finalized notes should append successfully");

                if sapling_nct
                    .frontier()
                    .map_or(false, SaplingNoteCommitmentTree::is_complete_subtree)
                {
                    break;
                }
            }

            sapling_nct
                .completed_subtree_index_and_root()
                .expect("already checked is_complete_subtree()")
        };

        let subtree = NoteCommitmentSubtree::new(index, height, node);

        let mut batch = DiskWriteBatch::new();

        batch.insert_sapling_subtree(upgrade_db, subtree);

        upgrade_db
            .write_batch(batch)
            .expect("writing sapling note commitment subtrees should always succeed.");

        subtree_count += 1;
        prev_tree = Some(tree);
    }

    let mut subtree_count = 0;
    let mut prev_tree: Option<_> = None;
    for (height, tree) in upgrade_db.orchard_tree_by_height_range(..initial_tip_height) {
        if should_cancel_format_change.load(atomic::Ordering::Relaxed) {
            break;
        }

        let Some(frontier) = tree.frontier() else {
            prev_tree = Some(tree);
            continue
        };

        // Blocks cannot complete multiple level 16 subtrees,
        // the subtree index can increase by a maximum of 1 every ~20 blocks.
        let subtree_address = OrchardNoteCommitmentTree::subtree_address(frontier);
        if subtree_address.index() <= subtree_count {
            prev_tree = Some(tree);
            continue;
        }

        let (index, node) = if OrchardNoteCommitmentTree::is_complete_subtree(frontier) {
            tree.completed_subtree_index_and_root()
                .expect("already checked is_complete_subtree()")
        } else {
            let mut orchard_nct = Arc::try_unwrap(
                prev_tree
                    .take()
                    .expect("should have some previous orchard frontier"),
            )
            .unwrap_or_else(|shared_tree| (*shared_tree).clone());

            let block = upgrade_db
                .block(height.into())
                .expect("height with note commitment tree should have block");

            let orchard_note_commitments: Vec<_> = block
                .transactions
                .iter()
                .flat_map(|tx| tx.orchard_note_commitments())
                .cloned()
                .collect();

            for orchard_note_commitment in orchard_note_commitments {
                orchard_nct
                    .append(orchard_note_commitment)
                    .expect("finalized notes should append successfully");

                if orchard_nct
                    .frontier()
                    .map_or(false, OrchardNoteCommitmentTree::is_complete_subtree)
                {
                    break;
                }
            }

            orchard_nct
                .completed_subtree_index_and_root()
                .expect("already checked is_complete_subtree()")
        };

        let subtree = NoteCommitmentSubtree::new(index, height, node);

        let mut batch = DiskWriteBatch::new();

        batch.insert_orchard_subtree(upgrade_db, subtree);

        upgrade_db
            .write_batch(batch)
            .expect("writing orchard note commitment subtrees should always succeed.");

        subtree_count += 1;
        prev_tree = Some(tree);
    }
}
