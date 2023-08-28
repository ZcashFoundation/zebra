//! Prunes duplicate Sapling and Orchard note commitment trees from database

use std::sync::mpsc;

use zebra_chain::block::Height;

use crate::service::finalized_state::{DiskWriteBatch, ZebraDb};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for pruning duplicate Sapling and Orchard note commitment trees from database
pub struct PruneTrees;

impl DiskFormatUpgrade for PruneTrees {
    fn run(
        &self,
        initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
    ) {
        // Prune duplicate Sapling note commitment trees.

        // The last tree we checked.
        let mut last_tree = db
            .sapling_tree_by_height(&Height(0))
            .expect("Checked above that the genesis block is in the database.");

        // Run through all the possible duplicate trees in the finalized chain.
        // The block after genesis is the first possible duplicate.
        for (height, tree) in db.sapling_tree_by_height_range(Height(1)..=initial_tip_height) {
            // Return early if there is a cancel signal.
            if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return;
            }

            // Delete any duplicate trees.
            if tree == last_tree {
                let mut batch = DiskWriteBatch::new();
                batch.delete_sapling_tree(db, &height);
                db.write_batch(batch)
                    .expect("Deleting Sapling note commitment trees should always succeed.");
            }

            // Compare against the last tree to find unique trees.
            last_tree = tree;
        }

        // Prune duplicate Orchard note commitment trees.

        // The last tree we checked.
        let mut last_tree = db
            .orchard_tree_by_height(&Height(0))
            .expect("Checked above that the genesis block is in the database.");

        // Run through all the possible duplicate trees in the finalized chain.
        // The block after genesis is the first possible duplicate.
        for (height, tree) in db.orchard_tree_by_height_range(Height(1)..=initial_tip_height) {
            // Return early if there is a cancel signal.
            if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return;
            }

            // Delete any duplicate trees.
            if tree == last_tree {
                let mut batch = DiskWriteBatch::new();
                batch.delete_orchard_tree(db, &height);
                db.write_batch(batch)
                    .expect("Deleting Orchard note commitment trees should always succeed.");
            }

            // Compare against the last tree to find unique trees.
            last_tree = tree;
        }
    }

    /// Check that note commitment trees were correctly de-duplicated.
    ///
    /// # Panics
    ///
    /// If a duplicate tree is found.
    fn validate(&self, db: &ZebraDb) {
        // Runtime test: make sure we removed all duplicates.
        // We always run this test, even if the state has supposedly been upgraded.
        let mut duplicate_found = false;

        let mut prev_height = None;
        let mut prev_tree = None;
        for (height, tree) in db.sapling_tree_by_height_range(..) {
            if prev_tree == Some(tree.clone()) {
                // TODO: replace this with a panic because it indicates an unrecoverable
                //       bug, which should fail the tests immediately
                error!(
                    height = ?height,
                    prev_height = ?prev_height.unwrap(),
                    tree_root = ?tree.root(),
                    "found duplicate sapling trees after running de-duplicate tree upgrade"
                );

                duplicate_found = true;
            }

            prev_height = Some(height);
            prev_tree = Some(tree);
        }

        let mut prev_height = None;
        let mut prev_tree = None;
        for (height, tree) in db.orchard_tree_by_height_range(..) {
            if prev_tree == Some(tree.clone()) {
                // TODO: replace this with a panic because it indicates an unrecoverable
                //       bug, which should fail the tests immediately
                error!(
                    height = ?height,
                    prev_height = ?prev_height.unwrap(),
                    tree_root = ?tree.root(),
                    "found duplicate orchard trees after running de-duplicate tree upgrade"
                );

                duplicate_found = true;
            }

            prev_height = Some(height);
            prev_tree = Some(tree);
        }

        if duplicate_found {
            panic!(
                "found duplicate sapling or orchard trees \
                     after running de-duplicate tree upgrade"
            );
        }
    }
}
