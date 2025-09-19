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
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use zebra_chain::{
    block::Height,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    sapling, sprout,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::Transaction,
};

use crate::{
    request::{FinalizedBlock, Treestate},
    service::finalized_state::{
        disk_db::{DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::RawBytes,
        zebra_db::ZebraDb,
    },
    BoxError, TransactionLocation,
};

// Doc-only items
#[allow(unused_imports)]
use zebra_chain::subtree::NoteCommitmentSubtree;

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

    /// Returns the [`TransactionLocation`] of the transaction that revealed
    /// the given [`sprout::Nullifier`], if it is revealed in the finalized state and its
    /// spending transaction hash has been indexed.
    #[allow(clippy::unwrap_in_result)]
    pub fn sprout_revealing_tx_loc(
        &self,
        sprout_nullifier: &sprout::Nullifier,
    ) -> Option<TransactionLocation> {
        let sprout_nullifiers = self.db.cf_handle("sprout_nullifiers").unwrap();
        self.db.zs_get(&sprout_nullifiers, &sprout_nullifier)?
    }

    /// Returns the [`TransactionLocation`] of the transaction that revealed
    /// the given [`sapling::Nullifier`], if it is revealed in the finalized state and its
    /// spending transaction hash has been indexed.
    #[allow(clippy::unwrap_in_result)]
    pub fn sapling_revealing_tx_loc(
        &self,
        sapling_nullifier: &sapling::Nullifier,
    ) -> Option<TransactionLocation> {
        let sapling_nullifiers = self.db.cf_handle("sapling_nullifiers").unwrap();
        self.db.zs_get(&sapling_nullifiers, &sapling_nullifier)?
    }

    /// Returns the [`TransactionLocation`] of the transaction that revealed
    /// the given [`orchard::Nullifier`], if it is revealed in the finalized state and its
    /// spending transaction hash has been indexed.
    #[allow(clippy::unwrap_in_result)]
    pub fn orchard_revealing_tx_loc(
        &self,
        orchard_nullifier: &orchard::Nullifier,
    ) -> Option<TransactionLocation> {
        let orchard_nullifiers = self.db.cf_handle("orchard_nullifiers").unwrap();
        self.db.zs_get(&orchard_nullifiers, &orchard_nullifier)?
    }

    /// Returns `true` if the finalized state contains `sprout_anchor`.
    #[allow(dead_code)]
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

    // # Sprout trees

    /// Returns the Sprout note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn sprout_tree_for_tip(&self) -> Arc<sprout::tree::NoteCommitmentTree> {
        if self.is_empty() {
            return Arc::<sprout::tree::NoteCommitmentTree>::default();
        }

        let sprout_tree_cf = self.db.cf_handle("sprout_note_commitment_tree").unwrap();

        // # Backwards Compatibility
        //
        // This code can read the column family format in 1.2.0 and earlier (tip height key),
        // and after PR #7392 is merged (empty key). The height-based code can be removed when
        // versions 1.2.0 and earlier are no longer supported.
        //
        // # Concurrency
        //
        // There is only one entry in this column family, which is atomically updated by a block
        // write batch (database transaction). If we used a height as the column family tree,
        // any updates between reading the tip height and reading the tree could cause panics.
        //
        // So we use the empty key `()`. Since the key has a constant value, we will always read
        // the latest tree.
        let mut sprout_tree: Option<Arc<sprout::tree::NoteCommitmentTree>> =
            self.db.zs_get(&sprout_tree_cf, &());

        if sprout_tree.is_none() {
            // In Zebra 1.4.0 and later, we don't update the sprout tip tree unless it is changed.
            // And we write with a `()` key, not a height key.
            // So we need to look for the most recent update height if the `()` key has never been written.
            sprout_tree = self
                .db
                .zs_last_key_value(&sprout_tree_cf)
                .map(|(_key, tree_value): (Height, _)| tree_value);
        }

        sprout_tree.expect("Sprout note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sprout note commitment tree matching the given anchor.
    ///
    /// This is used for interstitial tree building, which is unique to Sprout.
    #[allow(clippy::unwrap_in_result)]
    pub fn sprout_tree_by_anchor(
        &self,
        sprout_anchor: &sprout::tree::Root,
    ) -> Option<Arc<sprout::tree::NoteCommitmentTree>> {
        let sprout_anchors_handle = self.db.cf_handle("sprout_anchors").unwrap();

        self.db
            .zs_get(&sprout_anchors_handle, sprout_anchor)
            .map(Arc::new)
    }

    /// Returns all the Sprout note commitment trees in the database.
    ///
    /// Calling this method can load a lot of data into RAM, and delay block commit transactions.
    #[allow(dead_code)]
    pub fn sprout_trees_full_map(
        &self,
    ) -> HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>> {
        let sprout_anchors_handle = self.db.cf_handle("sprout_anchors").unwrap();

        self.db
            .zs_items_in_range_unordered(&sprout_anchors_handle, ..)
    }

    /// Returns all the Sprout note commitment tip trees.
    /// We only store the sprout tree for the tip, so this method is mainly used in tests.
    pub fn sprout_trees_full_tip(
        &self,
    ) -> impl Iterator<Item = (RawBytes, Arc<sprout::tree::NoteCommitmentTree>)> + '_ {
        let sprout_trees = self.db.cf_handle("sprout_note_commitment_tree").unwrap();
        self.db.zs_forward_range_iter(&sprout_trees, ..)
    }

    // # Sapling trees

    /// Returns the Sapling note commitment tree of the finalized tip or the empty tree if the state
    /// is empty.
    pub fn sapling_tree_for_tip(&self) -> Arc<sapling::tree::NoteCommitmentTree> {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };

        self.sapling_tree_by_height(&height)
            .expect("Sapling note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sapling note commitment tree matching the given block height, or `None` if the
    /// height is above the finalized tip.
    #[allow(clippy::unwrap_in_result)]
    pub fn sapling_tree_by_height(
        &self,
        height: &Height,
    ) -> Option<Arc<sapling::tree::NoteCommitmentTree>> {
        let tip_height = self.finalized_tip_height()?;

        // If we're above the tip, searching backwards would always return the tip tree.
        // But the correct answer is "we don't know that tree yet".
        if *height > tip_height {
            return None;
        }

        let sapling_trees = self.db.cf_handle("sapling_note_commitment_tree").unwrap();

        // If we know there must be a tree, search backwards for it.
        let (_first_duplicate_height, tree) = self
            .db
            .zs_prev_key_value_back_from(&sapling_trees, height)
            .expect(
                "Sapling note commitment trees must exist for all heights below the finalized tip",
            );

        Some(Arc::new(tree))
    }

    /// Returns the Sapling note commitment trees in the supplied range, in increasing height order.
    pub fn sapling_tree_by_height_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (Height, Arc<sapling::tree::NoteCommitmentTree>)> + '_
    where
        R: std::ops::RangeBounds<Height>,
    {
        let sapling_trees = self.db.cf_handle("sapling_note_commitment_tree").unwrap();
        self.db.zs_forward_range_iter(&sapling_trees, range)
    }

    /// Returns the Sapling note commitment trees in the reversed range, in decreasing height order.
    pub fn sapling_tree_by_reversed_height_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (Height, Arc<sapling::tree::NoteCommitmentTree>)> + '_
    where
        R: std::ops::RangeBounds<Height>,
    {
        let sapling_trees = self.db.cf_handle("sapling_note_commitment_tree").unwrap();
        self.db.zs_reverse_range_iter(&sapling_trees, range)
    }

    /// Returns the Sapling note commitment subtree at this `index`.
    ///
    /// # Correctness
    ///
    /// This method should not be used to get subtrees for RPC responses,
    /// because those subtree lists require that the start subtree is present in the list.
    /// Instead, use `sapling_subtree_list_by_index_for_rpc()`.
    #[allow(clippy::unwrap_in_result)]
    pub(in super::super) fn sapling_subtree_by_index(
        &self,
        index: impl Into<NoteCommitmentSubtreeIndex> + Copy,
    ) -> Option<NoteCommitmentSubtree<sapling_crypto::Node>> {
        let sapling_subtrees = self
            .db
            .cf_handle("sapling_note_commitment_subtree")
            .unwrap();

        let subtree_data: NoteCommitmentSubtreeData<sapling_crypto::Node> =
            self.db.zs_get(&sapling_subtrees, &index.into())?;

        Some(subtree_data.with_index(index))
    }

    /// Returns a list of Sapling [`NoteCommitmentSubtree`]s in the provided range.
    #[allow(clippy::unwrap_in_result)]
    pub fn sapling_subtree_list_by_index_range(
        &self,
        range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex>,
    ) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling_crypto::Node>> {
        let sapling_subtrees = self
            .db
            .cf_handle("sapling_note_commitment_subtree")
            .unwrap();

        self.db
            .zs_forward_range_iter(&sapling_subtrees, range)
            .collect()
    }

    /// Get the sapling note commitment subtress for the finalized tip.
    #[allow(clippy::unwrap_in_result)]
    fn sapling_subtree_for_tip(&self) -> Option<NoteCommitmentSubtree<sapling_crypto::Node>> {
        let sapling_subtrees = self
            .db
            .cf_handle("sapling_note_commitment_subtree")
            .unwrap();

        let (index, subtree_data): (
            NoteCommitmentSubtreeIndex,
            NoteCommitmentSubtreeData<sapling_crypto::Node>,
        ) = self.db.zs_last_key_value(&sapling_subtrees)?;

        let tip_height = self.finalized_tip_height()?;
        if subtree_data.end_height != tip_height {
            return None;
        }

        Some(subtree_data.with_index(index))
    }

    // Orchard trees

    /// Returns the Orchard note commitment tree of the finalized tip or the empty tree if the state
    /// is empty.
    pub fn orchard_tree_for_tip(&self) -> Arc<orchard::tree::NoteCommitmentTree> {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };

        self.orchard_tree_by_height(&height)
            .expect("Orchard note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Orchard note commitment tree matching the given block height,
    /// or `None` if the height is above the finalized tip.
    #[allow(clippy::unwrap_in_result)]
    pub fn orchard_tree_by_height(
        &self,
        height: &Height,
    ) -> Option<Arc<orchard::tree::NoteCommitmentTree>> {
        let tip_height = self.finalized_tip_height()?;

        // If we're above the tip, searching backwards would always return the tip tree.
        // But the correct answer is "we don't know that tree yet".
        if *height > tip_height {
            return None;
        }

        let orchard_trees = self.db.cf_handle("orchard_note_commitment_tree").unwrap();

        // If we know there must be a tree, search backwards for it.
        let (_first_duplicate_height, tree) = self
            .db
            .zs_prev_key_value_back_from(&orchard_trees, height)
            .expect(
                "Orchard note commitment trees must exist for all heights below the finalized tip",
            );

        Some(Arc::new(tree))
    }

    /// Returns the Orchard note commitment trees in the supplied range, in increasing height order.
    pub fn orchard_tree_by_height_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (Height, Arc<orchard::tree::NoteCommitmentTree>)> + '_
    where
        R: std::ops::RangeBounds<Height>,
    {
        let orchard_trees = self.db.cf_handle("orchard_note_commitment_tree").unwrap();
        self.db.zs_forward_range_iter(&orchard_trees, range)
    }

    /// Returns the Orchard note commitment trees in the reversed range, in decreasing height order.
    pub fn orchard_tree_by_reversed_height_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (Height, Arc<orchard::tree::NoteCommitmentTree>)> + '_
    where
        R: std::ops::RangeBounds<Height>,
    {
        let orchard_trees = self.db.cf_handle("orchard_note_commitment_tree").unwrap();
        self.db.zs_reverse_range_iter(&orchard_trees, range)
    }

    /// Returns the Orchard note commitment subtree at this `index`.
    ///
    /// # Correctness
    ///
    /// This method should not be used to get subtrees for RPC responses,
    /// because those subtree lists require that the start subtree is present in the list.
    /// Instead, use `orchard_subtree_list_by_index_for_rpc()`.
    #[allow(clippy::unwrap_in_result)]
    pub(in super::super) fn orchard_subtree_by_index(
        &self,
        index: impl Into<NoteCommitmentSubtreeIndex> + Copy,
    ) -> Option<NoteCommitmentSubtree<orchard::tree::Node>> {
        let orchard_subtrees = self
            .db
            .cf_handle("orchard_note_commitment_subtree")
            .unwrap();

        let subtree_data: NoteCommitmentSubtreeData<orchard::tree::Node> =
            self.db.zs_get(&orchard_subtrees, &index.into())?;

        Some(subtree_data.with_index(index))
    }

    /// Returns a list of Orchard [`NoteCommitmentSubtree`]s in the provided range.
    #[allow(clippy::unwrap_in_result)]
    pub fn orchard_subtree_list_by_index_range(
        &self,
        range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex>,
    ) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>> {
        let orchard_subtrees = self
            .db
            .cf_handle("orchard_note_commitment_subtree")
            .unwrap();

        self.db
            .zs_forward_range_iter(&orchard_subtrees, range)
            .collect()
    }

    /// Get the orchard note commitment subtress for the finalized tip.
    #[allow(clippy::unwrap_in_result)]
    fn orchard_subtree_for_tip(&self) -> Option<NoteCommitmentSubtree<orchard::tree::Node>> {
        let orchard_subtrees = self
            .db
            .cf_handle("orchard_note_commitment_subtree")
            .unwrap();

        let (index, subtree_data): (
            NoteCommitmentSubtreeIndex,
            NoteCommitmentSubtreeData<orchard::tree::Node>,
        ) = self.db.zs_last_key_value(&orchard_subtrees)?;

        let tip_height = self.finalized_tip_height()?;
        if subtree_data.end_height != tip_height {
            return None;
        }

        Some(subtree_data.with_index(index))
    }

    /// Returns the shielded note commitment trees of the finalized tip
    /// or the empty trees if the state is empty.
    /// Additionally, returns the sapling and orchard subtrees for the finalized tip if
    /// the current subtree is finalizing in the tip, None otherwise.
    pub fn note_commitment_trees_for_tip(&self) -> NoteCommitmentTrees {
        NoteCommitmentTrees {
            sprout: self.sprout_tree_for_tip(),
            sapling: self.sapling_tree_for_tip(),
            sapling_subtree: self.sapling_subtree_for_tip(),
            orchard: self.orchard_tree_for_tip(),
            orchard_subtree: self.orchard_subtree_for_tip(),
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
        zebra_db: &ZebraDb,
        finalized: &FinalizedBlock,
    ) -> Result<(), BoxError> {
        #[cfg(feature = "indexer")]
        let FinalizedBlock { block, height, .. } = finalized;

        // Index each transaction's shielded data
        #[cfg(feature = "indexer")]
        for (tx_index, transaction) in block.transactions.iter().enumerate() {
            let tx_loc = TransactionLocation::from_usize(*height, tx_index);
            self.prepare_nullifier_batch(zebra_db, transaction, tx_loc)?;
        }

        #[cfg(not(feature = "indexer"))]
        for transaction in &finalized.block.transactions {
            self.prepare_nullifier_batch(zebra_db, transaction)?;
        }

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
        zebra_db: &ZebraDb,
        transaction: &Transaction,
        #[cfg(feature = "indexer")] transaction_location: TransactionLocation,
    ) -> Result<(), BoxError> {
        let db = &zebra_db.db;
        let sprout_nullifiers = db.cf_handle("sprout_nullifiers").unwrap();
        let sapling_nullifiers = db.cf_handle("sapling_nullifiers").unwrap();
        let orchard_nullifiers = db.cf_handle("orchard_nullifiers").unwrap();

        #[cfg(feature = "indexer")]
        let insert_value = transaction_location;
        #[cfg(not(feature = "indexer"))]
        let insert_value = ();

        // Mark sprout, sapling and orchard nullifiers as spent
        for sprout_nullifier in transaction.sprout_nullifiers() {
            self.zs_insert(&sprout_nullifiers, sprout_nullifier, insert_value);
        }
        for sapling_nullifier in transaction.sapling_nullifiers() {
            self.zs_insert(&sapling_nullifiers, sapling_nullifier, insert_value);
        }
        for orchard_nullifier in transaction.orchard_nullifiers() {
            self.zs_insert(&orchard_nullifiers, orchard_nullifier, insert_value);
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
    pub fn prepare_trees_batch(
        &mut self,
        zebra_db: &ZebraDb,
        finalized: &FinalizedBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
    ) -> Result<(), BoxError> {
        let FinalizedBlock {
            height,
            treestate:
                Treestate {
                    note_commitment_trees,
                    history_tree,
                },
            ..
        } = finalized;

        let prev_sprout_tree = prev_note_commitment_trees.as_ref().map_or_else(
            || zebra_db.sprout_tree_for_tip(),
            |prev_trees| prev_trees.sprout.clone(),
        );
        let prev_sapling_tree = prev_note_commitment_trees.as_ref().map_or_else(
            || zebra_db.sapling_tree_for_tip(),
            |prev_trees| prev_trees.sapling.clone(),
        );
        let prev_orchard_tree = prev_note_commitment_trees.as_ref().map_or_else(
            || zebra_db.orchard_tree_for_tip(),
            |prev_trees| prev_trees.orchard.clone(),
        );

        // Update the Sprout tree and store its anchor only if it has changed
        if height.is_min() || prev_sprout_tree != note_commitment_trees.sprout {
            self.update_sprout_tree(zebra_db, &note_commitment_trees.sprout)
        }

        // Store the Sapling tree, anchor, and any new subtrees only if they have changed
        if height.is_min() || prev_sapling_tree != note_commitment_trees.sapling {
            self.create_sapling_tree(zebra_db, height, &note_commitment_trees.sapling);

            if let Some(subtree) = note_commitment_trees.sapling_subtree {
                self.insert_sapling_subtree(zebra_db, &subtree);
            }
        }

        // Store the Orchard tree, anchor, and any new subtrees only if they have changed
        if height.is_min() || prev_orchard_tree != note_commitment_trees.orchard {
            self.create_orchard_tree(zebra_db, height, &note_commitment_trees.orchard);

            if let Some(subtree) = note_commitment_trees.orchard_subtree {
                self.insert_orchard_subtree(zebra_db, &subtree);
            }
        }

        self.update_history_tree(zebra_db, history_tree);

        Ok(())
    }

    // Sprout tree methods

    /// Updates the Sprout note commitment tree for the tip, and the Sprout anchors.
    pub fn update_sprout_tree(
        &mut self,
        zebra_db: &ZebraDb,
        tree: &sprout::tree::NoteCommitmentTree,
    ) {
        let sprout_anchors = zebra_db.db.cf_handle("sprout_anchors").unwrap();
        let sprout_tree_cf = zebra_db
            .db
            .cf_handle("sprout_note_commitment_tree")
            .unwrap();

        // Sprout lookups need all previous trees by their anchors.
        // The root must be calculated first, so it is cached in the database.
        self.zs_insert(&sprout_anchors, tree.root(), tree);
        self.zs_insert(&sprout_tree_cf, (), tree);
    }

    /// Legacy method: Deletes the range of Sprout note commitment trees at the given [`Height`]s.
    /// Doesn't delete anchors from the anchor index. Doesn't delete the upper bound.
    ///
    /// From state format 25.3.0 onwards, the Sprout trees are indexed by an empty key,
    /// so this method does nothing.
    pub fn delete_range_sprout_tree(&mut self, zebra_db: &ZebraDb, from: &Height, to: &Height) {
        let sprout_tree_cf = zebra_db
            .db
            .cf_handle("sprout_note_commitment_tree")
            .unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&sprout_tree_cf, from, to);
    }

    /// Deletes the given Sprout note commitment tree `anchor`.
    #[allow(dead_code)]
    pub fn delete_sprout_anchor(&mut self, zebra_db: &ZebraDb, anchor: &sprout::tree::Root) {
        let sprout_anchors = zebra_db.db.cf_handle("sprout_anchors").unwrap();
        self.zs_delete(&sprout_anchors, anchor);
    }

    // Sapling tree methods

    /// Inserts or overwrites the Sapling note commitment tree at the given [`Height`],
    /// and the Sapling anchors.
    pub fn create_sapling_tree(
        &mut self,
        zebra_db: &ZebraDb,
        height: &Height,
        tree: &sapling::tree::NoteCommitmentTree,
    ) {
        let sapling_anchors = zebra_db.db.cf_handle("sapling_anchors").unwrap();
        let sapling_tree_cf = zebra_db
            .db
            .cf_handle("sapling_note_commitment_tree")
            .unwrap();

        self.zs_insert(&sapling_anchors, tree.root(), ());
        self.zs_insert(&sapling_tree_cf, height, tree);
    }

    /// Inserts the Sapling note commitment subtree into the batch.
    pub fn insert_sapling_subtree(
        &mut self,
        zebra_db: &ZebraDb,
        subtree: &NoteCommitmentSubtree<sapling_crypto::Node>,
    ) {
        let sapling_subtree_cf = zebra_db
            .db
            .cf_handle("sapling_note_commitment_subtree")
            .unwrap();
        self.zs_insert(&sapling_subtree_cf, subtree.index, subtree.into_data());
    }

    /// Deletes the Sapling note commitment tree at the given [`Height`].
    pub fn delete_sapling_tree(&mut self, zebra_db: &ZebraDb, height: &Height) {
        let sapling_tree_cf = zebra_db
            .db
            .cf_handle("sapling_note_commitment_tree")
            .unwrap();
        self.zs_delete(&sapling_tree_cf, height);
    }

    /// Deletes the range of Sapling note commitment trees at the given [`Height`]s.
    /// Doesn't delete anchors from the anchor index. Doesn't delete the upper bound.
    #[allow(dead_code)]
    pub fn delete_range_sapling_tree(&mut self, zebra_db: &ZebraDb, from: &Height, to: &Height) {
        let sapling_tree_cf = zebra_db
            .db
            .cf_handle("sapling_note_commitment_tree")
            .unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&sapling_tree_cf, from, to);
    }

    /// Deletes the given Sapling note commitment tree `anchor`.
    #[allow(dead_code)]
    pub fn delete_sapling_anchor(&mut self, zebra_db: &ZebraDb, anchor: &sapling::tree::Root) {
        let sapling_anchors = zebra_db.db.cf_handle("sapling_anchors").unwrap();
        self.zs_delete(&sapling_anchors, anchor);
    }

    /// Deletes the range of Sapling subtrees at the given [`NoteCommitmentSubtreeIndex`]es.
    /// Doesn't delete the upper bound.
    pub fn delete_range_sapling_subtree(
        &mut self,
        zebra_db: &ZebraDb,
        from: NoteCommitmentSubtreeIndex,
        to: NoteCommitmentSubtreeIndex,
    ) {
        let sapling_subtree_cf = zebra_db
            .db
            .cf_handle("sapling_note_commitment_subtree")
            .unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&sapling_subtree_cf, from, to);
    }

    // Orchard tree methods

    /// Inserts or overwrites the Orchard note commitment tree at the given [`Height`],
    /// and the Orchard anchors.
    pub fn create_orchard_tree(
        &mut self,
        zebra_db: &ZebraDb,
        height: &Height,
        tree: &orchard::tree::NoteCommitmentTree,
    ) {
        let orchard_anchors = zebra_db.db.cf_handle("orchard_anchors").unwrap();
        let orchard_tree_cf = zebra_db
            .db
            .cf_handle("orchard_note_commitment_tree")
            .unwrap();

        self.zs_insert(&orchard_anchors, tree.root(), ());
        self.zs_insert(&orchard_tree_cf, height, tree);
    }

    /// Inserts the Orchard note commitment subtree into the batch.
    pub fn insert_orchard_subtree(
        &mut self,
        zebra_db: &ZebraDb,
        subtree: &NoteCommitmentSubtree<orchard::tree::Node>,
    ) {
        let orchard_subtree_cf = zebra_db
            .db
            .cf_handle("orchard_note_commitment_subtree")
            .unwrap();
        self.zs_insert(&orchard_subtree_cf, subtree.index, subtree.into_data());
    }

    /// Deletes the Orchard note commitment tree at the given [`Height`].
    pub fn delete_orchard_tree(&mut self, zebra_db: &ZebraDb, height: &Height) {
        let orchard_tree_cf = zebra_db
            .db
            .cf_handle("orchard_note_commitment_tree")
            .unwrap();
        self.zs_delete(&orchard_tree_cf, height);
    }

    /// Deletes the range of Orchard note commitment trees at the given [`Height`]s.
    /// Doesn't delete anchors from the anchor index. Doesn't delete the upper bound.
    #[allow(dead_code)]
    pub fn delete_range_orchard_tree(&mut self, zebra_db: &ZebraDb, from: &Height, to: &Height) {
        let orchard_tree_cf = zebra_db
            .db
            .cf_handle("orchard_note_commitment_tree")
            .unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&orchard_tree_cf, from, to);
    }

    /// Deletes the given Orchard note commitment tree `anchor`.
    #[allow(dead_code)]
    pub fn delete_orchard_anchor(&mut self, zebra_db: &ZebraDb, anchor: &orchard::tree::Root) {
        let orchard_anchors = zebra_db.db.cf_handle("orchard_anchors").unwrap();
        self.zs_delete(&orchard_anchors, anchor);
    }

    /// Deletes the range of Orchard subtrees at the given [`NoteCommitmentSubtreeIndex`]es.
    /// Doesn't delete the upper bound.
    pub fn delete_range_orchard_subtree(
        &mut self,
        zebra_db: &ZebraDb,
        from: NoteCommitmentSubtreeIndex,
        to: NoteCommitmentSubtreeIndex,
    ) {
        let orchard_subtree_cf = zebra_db
            .db
            .cf_handle("orchard_note_commitment_subtree")
            .unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&orchard_subtree_cf, from, to);
    }
}
