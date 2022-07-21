//! Provides high-level access to database shielded:
//! - nullifiers
//! - note commitment trees
//! - anchors
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    history_tree::HistoryTree,
    orchard, sapling, sprout,
    transaction::Transaction,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        zebra_db::ZebraDb,
        FinalizedBlock,
    },
    BoxError,
};

/// An argument wrapper struct for note commitment trees.
#[derive(Clone, Debug)]
pub struct NoteCommitmentTrees {
    sprout: Arc<sprout::tree::NoteCommitmentTree>,
    sapling: Arc<sapling::tree::NoteCommitmentTree>,
    orchard: Arc<orchard::tree::NoteCommitmentTree>,
}

impl ZebraDb {
    // Read shielded methods

    /// Returns `true` if the finalized state contains `sprout_nullifier`.
    pub fn contains_sprout_nullifier(&self, sprout_nullifier: &sprout::Nullifier) -> bool {
        let sprout_nullifiers = self.db.cf_handle("sprout_nullifiers").unwrap();
        self.db.zs_contains(&sprout_nullifiers, &sprout_nullifier)
    }

    /// Returns `true` if the finalized state contains `sapling_nullifier`.
    pub fn contains_sapling_nullifier(&self, sapling_nullifier: &sapling::Nullifier) -> bool {
        let sapling_nullifiers = self.db.cf_handle("sapling_nullifiers").unwrap();
        self.db.zs_contains(&sapling_nullifiers, &sapling_nullifier)
    }

    /// Returns `true` if the finalized state contains `orchard_nullifier`.
    pub fn contains_orchard_nullifier(&self, orchard_nullifier: &orchard::Nullifier) -> bool {
        let orchard_nullifiers = self.db.cf_handle("orchard_nullifiers").unwrap();
        self.db.zs_contains(&orchard_nullifiers, &orchard_nullifier)
    }

    /// Returns `true` if the finalized state contains `sprout_anchor`.
    #[allow(unused)]
    pub fn contains_sprout_anchor(&self, sprout_anchor: &sprout::tree::Root) -> bool {
        let sprout_anchors = self.db.cf_handle("sprout_anchors").unwrap();
        self.db.zs_contains(&sprout_anchors, &sprout_anchor)
    }

    /// Returns `true` if the finalized state contains `sapling_anchor`.
    pub fn contains_sapling_anchor(&self, sapling_anchor: &sapling::tree::Root) -> bool {
        let sapling_anchors = self.db.cf_handle("sapling_anchors").unwrap();
        self.db.zs_contains(&sapling_anchors, &sapling_anchor)
    }

    /// Returns `true` if the finalized state contains `orchard_anchor`.
    pub fn contains_orchard_anchor(&self, orchard_anchor: &orchard::tree::Root) -> bool {
        let orchard_anchors = self.db.cf_handle("orchard_anchors").unwrap();
        self.db.zs_contains(&orchard_anchors, &orchard_anchor)
    }

    /// Returns the Sprout note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn sprout_note_commitment_tree(&self) -> Arc<sprout::tree::NoteCommitmentTree> {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };

        let sprout_nct_handle = self.db.cf_handle("sprout_note_commitment_tree").unwrap();

        self.db
            .zs_get(&sprout_nct_handle, &height)
            .map(Arc::new)
            .expect("Sprout note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sprout note commitment tree matching the given anchor.
    ///
    /// This is used for interstitial tree building, which is unique to Sprout.
    #[allow(clippy::unwrap_in_result)]
    pub fn sprout_note_commitment_tree_by_anchor(
        &self,
        sprout_anchor: &sprout::tree::Root,
    ) -> Option<Arc<sprout::tree::NoteCommitmentTree>> {
        let sprout_anchors_handle = self.db.cf_handle("sprout_anchors").unwrap();

        self.db
            .zs_get(&sprout_anchors_handle, sprout_anchor)
            .map(Arc::new)
    }

    /// Returns the Sapling note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn sapling_note_commitment_tree(&self) -> Arc<sapling::tree::NoteCommitmentTree> {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };

        let sapling_nct_handle = self.db.cf_handle("sapling_note_commitment_tree").unwrap();

        self.db
            .zs_get(&sapling_nct_handle, &height)
            .map(Arc::new)
            .expect("Sapling note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sapling note commitment tree matching the given block height.
    #[allow(dead_code)]
    #[allow(clippy::unwrap_in_result)]
    pub fn sapling_note_commitment_tree_by_height(
        &self,
        height: &Height,
    ) -> Option<Arc<sapling::tree::NoteCommitmentTree>> {
        let sapling_trees = self.db.cf_handle("sapling_note_commitment_tree").unwrap();

        self.db.zs_get(&sapling_trees, height).map(Arc::new)
    }

    /// Returns the Orchard note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn orchard_note_commitment_tree(&self) -> Arc<orchard::tree::NoteCommitmentTree> {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };

        let orchard_nct_handle = self.db.cf_handle("orchard_note_commitment_tree").unwrap();

        self.db
            .zs_get(&orchard_nct_handle, &height)
            .map(Arc::new)
            .expect("Orchard note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Orchard note commitment tree matching the given block height.
    #[allow(dead_code)]
    #[allow(clippy::unwrap_in_result)]
    pub fn orchard_note_commitment_tree_by_height(
        &self,
        height: &Height,
    ) -> Option<Arc<orchard::tree::NoteCommitmentTree>> {
        let orchard_trees = self.db.cf_handle("orchard_note_commitment_tree").unwrap();

        self.db.zs_get(&orchard_trees, height).map(Arc::new)
    }

    /// Returns the shielded note commitment trees of the finalized tip
    /// or the empty trees if the state is empty.
    pub fn note_commitment_trees(&self) -> NoteCommitmentTrees {
        NoteCommitmentTrees {
            sprout: self.sprout_note_commitment_tree(),
            sapling: self.sapling_note_commitment_tree(),
            orchard: self.orchard_note_commitment_tree(),
        }
    }
}

impl DiskWriteBatch {
    /// Prepare a database batch containing `finalized.block`'s shielded transaction indexes,
    /// and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating note commitment trees
    pub fn prepare_shielded_transaction_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
        note_commitment_trees: &mut NoteCommitmentTrees,
    ) -> Result<(), BoxError> {
        let FinalizedBlock { block, .. } = finalized;

        // Index each transaction's shielded data
        for transaction in &block.transactions {
            self.prepare_nullifier_batch(db, transaction)?;
        }

        DiskWriteBatch::update_note_commitment_trees_parallel(block, note_commitment_trees)?;

        Ok(())
    }

    /// Prepare a database batch containing `finalized.block`'s nullifiers,
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_nullifier_batch(
        &mut self,
        db: &DiskDb,
        transaction: &Transaction,
    ) -> Result<(), BoxError> {
        let sprout_nullifiers = db.cf_handle("sprout_nullifiers").unwrap();
        let sapling_nullifiers = db.cf_handle("sapling_nullifiers").unwrap();
        let orchard_nullifiers = db.cf_handle("orchard_nullifiers").unwrap();

        // Mark sprout, sapling and orchard nullifiers as spent
        for sprout_nullifier in transaction.sprout_nullifiers() {
            self.zs_insert(&sprout_nullifiers, sprout_nullifier, ());
        }
        for sapling_nullifier in transaction.sapling_nullifiers() {
            self.zs_insert(&sapling_nullifiers, sapling_nullifier, ());
        }
        for orchard_nullifier in transaction.orchard_nullifiers() {
            self.zs_insert(&orchard_nullifiers, orchard_nullifier, ());
        }

        Ok(())
    }

    /// Update the supplied note commitment trees for the entire block,
    /// using parallel `rayon` threads.
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating note commitment trees
    pub fn update_note_commitment_trees_parallel(
        block: &Arc<Block>,
        note_commitment_trees: &mut NoteCommitmentTrees,
    ) -> Result<(), BoxError> {
        // Prepare arguments for parallel threads
        let NoteCommitmentTrees {
            sprout,
            sapling,
            orchard,
        } = note_commitment_trees.clone();

        let sprout_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.sprout_note_commitments())
            .cloned()
            .collect();
        let sapling_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.sapling_note_commitments())
            .cloned()
            .collect();
        let orchard_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.orchard_note_commitments())
            .cloned()
            .collect();

        let mut sprout_result = None;
        let mut sapling_result = None;
        let mut orchard_result = None;

        rayon::in_place_scope_fifo(|scope| {
            if !sprout_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    sprout_result = Some(Self::update_sprout_note_commitment_tree(
                        sprout,
                        sprout_note_commitments,
                    ));
                });
            }

            if !sapling_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    sapling_result = Some(Self::update_sapling_note_commitment_tree(
                        sapling,
                        sapling_note_commitments,
                    ));
                });
            }

            if !orchard_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    orchard_result = Some(Self::update_orchard_note_commitment_tree(
                        orchard,
                        orchard_note_commitments,
                    ));
                });
            }
        });

        if let Some(sprout_result) = sprout_result {
            note_commitment_trees.sprout = sprout_result?;
        }
        if let Some(sapling_result) = sapling_result {
            note_commitment_trees.sapling = sapling_result?;
        }
        if let Some(orchard_result) = orchard_result {
            note_commitment_trees.orchard = orchard_result?;
        }

        Ok(())
    }

    /// Update the sprout note commitment tree.
    fn update_sprout_note_commitment_tree(
        mut sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sprout_note_commitments: Vec<sprout::NoteCommitment>,
    ) -> Result<Arc<sprout::tree::NoteCommitmentTree>, BoxError> {
        let sprout_nct = Arc::make_mut(&mut sprout);

        for sprout_note_commitment in sprout_note_commitments {
            sprout_nct.append(sprout_note_commitment)?;
        }

        Ok(sprout)
    }

    /// Update the sapling note commitment tree.
    fn update_sapling_note_commitment_tree(
        mut sapling: Arc<sapling::tree::NoteCommitmentTree>,
        sapling_note_commitments: Vec<sapling::tree::NoteCommitmentUpdate>,
    ) -> Result<Arc<sapling::tree::NoteCommitmentTree>, BoxError> {
        let sapling_nct = Arc::make_mut(&mut sapling);

        for sapling_note_commitment in sapling_note_commitments {
            sapling_nct.append(sapling_note_commitment)?;
        }

        Ok(sapling)
    }

    /// Update the orchard note commitment tree.
    fn update_orchard_note_commitment_tree(
        mut orchard: Arc<orchard::tree::NoteCommitmentTree>,
        orchard_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<Arc<orchard::tree::NoteCommitmentTree>, BoxError> {
        let orchard_nct = Arc::make_mut(&mut orchard);

        for orchard_note_commitment in orchard_note_commitments {
            orchard_nct.append(orchard_note_commitment)?;
        }

        Ok(orchard)
    }

    /// Prepare a database batch containing the note commitment and history tree updates
    /// from `finalized.block`, and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating the history tree
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_note_commitment_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
        note_commitment_trees: NoteCommitmentTrees,
        history_tree: Arc<HistoryTree>,
    ) -> Result<(), BoxError> {
        let sprout_anchors = db.cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = db.cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = db.cf_handle("orchard_anchors").unwrap();

        let sprout_note_commitment_tree_cf = db.cf_handle("sprout_note_commitment_tree").unwrap();
        let sapling_note_commitment_tree_cf = db.cf_handle("sapling_note_commitment_tree").unwrap();
        let orchard_note_commitment_tree_cf = db.cf_handle("orchard_note_commitment_tree").unwrap();

        let FinalizedBlock { height, .. } = finalized;

        let (sprout_root, sapling_root, orchard_root) =
            Self::note_commitment_roots_parallel(note_commitment_trees.clone());

        // Compute the new anchors and index them
        // Note: if the root hasn't changed, we write the same value again.
        self.zs_insert(&sprout_anchors, sprout_root, &note_commitment_trees.sprout);
        self.zs_insert(&sapling_anchors, sapling_root, ());
        self.zs_insert(&orchard_anchors, orchard_root, ());

        // Delete the previously stored Sprout note commitment tree.
        let current_tip_height = *height - 1;
        if let Some(h) = current_tip_height {
            self.zs_delete(&sprout_note_commitment_tree_cf, h);
        }

        // TODO: if we ever need concurrent read-only access to the sprout tree,
        // store it by `()`, not height. Otherwise, the ReadStateService could
        // access a height that was just deleted by a concurrent StateService
        // write. This requires a database version update.
        self.zs_insert(
            &sprout_note_commitment_tree_cf,
            height,
            note_commitment_trees.sprout,
        );

        self.zs_insert(
            &sapling_note_commitment_tree_cf,
            height,
            note_commitment_trees.sapling,
        );

        self.zs_insert(
            &orchard_note_commitment_tree_cf,
            height,
            note_commitment_trees.orchard,
        );

        self.prepare_history_batch(db, finalized, sapling_root, orchard_root, history_tree)
    }

    /// Calculate the note commitment tree roots using parallel `rayon` threads.
    fn note_commitment_roots_parallel(
        note_commitment_trees: NoteCommitmentTrees,
    ) -> (sprout::tree::Root, sapling::tree::Root, orchard::tree::Root) {
        let NoteCommitmentTrees {
            sprout,
            sapling,
            orchard,
        } = note_commitment_trees;

        let mut sprout_root = None;
        let mut sapling_root = None;
        let mut orchard_root = None;

        // TODO: consider doing these calculations after the trees are updated,
        //       in the rayon scopes for each tree.
        //       To do this, we need to make sure we only calculate the roots once.
        rayon::in_place_scope_fifo(|scope| {
            scope.spawn_fifo(|_scope| {
                sprout_root = Some(sprout.root());
            });

            scope.spawn_fifo(|_scope| {
                sapling_root = Some(sapling.root());
            });

            scope.spawn_fifo(|_scope| {
                orchard_root = Some(orchard.root());
            });
        });

        (
            sprout_root.expect("scope has finished"),
            sapling_root.expect("scope has finished"),
            orchard_root.expect("scope has finished"),
        )
    }

    /// Prepare a database batch containing the initial note commitment trees,
    /// and return it (without actually writing anything).
    ///
    /// This method never returns an error.
    pub fn prepare_genesis_note_commitment_tree_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
    ) {
        let sprout_note_commitment_tree_cf = db.cf_handle("sprout_note_commitment_tree").unwrap();
        let sapling_note_commitment_tree_cf = db.cf_handle("sapling_note_commitment_tree").unwrap();
        let orchard_note_commitment_tree_cf = db.cf_handle("orchard_note_commitment_tree").unwrap();

        let FinalizedBlock { height, .. } = finalized;

        // Insert empty note commitment trees. Note that these can't be
        // used too early (e.g. the Orchard tree before Nu5 activates)
        // since the block validation will make sure only appropriate
        // transactions are allowed in a block.
        self.zs_insert(
            &sprout_note_commitment_tree_cf,
            height,
            sprout::tree::NoteCommitmentTree::default(),
        );
        self.zs_insert(
            &sapling_note_commitment_tree_cf,
            height,
            sapling::tree::NoteCommitmentTree::default(),
        );
        self.zs_insert(
            &orchard_note_commitment_tree_cf,
            height,
            orchard::tree::NoteCommitmentTree::default(),
        );
    }
}
