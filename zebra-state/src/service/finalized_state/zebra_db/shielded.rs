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
    block::Height, history_tree::HistoryTree, orchard, parallel::tree::NoteCommitmentTrees,
    sapling, sprout, transaction::Transaction,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        zebra_db::ZebraDb,
        FinalizedBlock,
    },
    BoxError,
};

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

        note_commitment_trees.update_trees_parallel(block)?;

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

        // Use the cached values that were previously calculated in parallel.
        let sprout_root = note_commitment_trees.sprout.root();
        let sapling_root = note_commitment_trees.sapling.root();
        let orchard_root = note_commitment_trees.orchard.root();

        // Index the new anchors.
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
