//! Provides high-level access to database whole-chain:
//! - history trees
//! - chain value pools
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{borrow::Borrow, collections::HashMap, sync::Arc};

use zebra_chain::{
    amount::NonNegative, block::Height, history_tree::HistoryTree, transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::RawBytes,
        zebra_db::ZebraDb,
    },
    BoxError, SemanticallyVerifiedBlock,
};

impl ZebraDb {
    // History tree methods

    /// Returns the ZIP-221 history tree of the finalized tip.
    ///
    /// If history trees have not been activated yet (pre-Heartwood), or the state is empty,
    /// returns an empty history tree.
    pub fn history_tree(&self) -> Arc<HistoryTree> {
        if self.is_empty() {
            return Arc::<HistoryTree>::default();
        }

        let history_tree_cf = self.db.cf_handle("history_tree").unwrap();

        // # Backwards Compatibility
        //
        // This code can read the column family format in 1.2.0 and earlier (tip height key),
        // and after PR #7392 is merged (empty key). The height-based code can be removed when
        // versions 1.2.0 and earlier are no longer supported.
        //
        // # Concurrency
        //
        // There is only one entry in this column family, which is atomically updated by a block
        // write batch (database transaction). If we used a height as the key in this column family,
        // any updates between reading the tip height and reading the tree could cause panics.
        //
        // So we use the empty key `()`. Since the key has a constant value, we will always read
        // the latest tree.
        let mut history_tree: Option<Arc<HistoryTree>> = self.db.zs_get(&history_tree_cf, &());

        if history_tree.is_none() {
            let tip_height = self
                .finalized_tip_height()
                .expect("just checked for an empty database");

            history_tree = self.db.zs_get(&history_tree_cf, &tip_height);
        }

        history_tree.unwrap_or_default()
    }

    /// Returns all the history tip trees.
    /// We only store the history tree for the tip, so this method is mainly used in tests.
    pub fn history_trees_full_tip(
        &self,
    ) -> impl Iterator<Item = (RawBytes, Arc<HistoryTree>)> + '_ {
        let history_tree_cf = self.db.cf_handle("history_tree").unwrap();

        self.db.zs_range_iter(&history_tree_cf, .., false)
    }

    // Value pool methods

    /// Returns the stored `ValueBalance` for the best chain at the finalized tip height.
    pub fn finalized_value_pool(&self) -> ValueBalance<NonNegative> {
        let value_pool_cf = self.db.cf_handle("tip_chain_value_pool").unwrap();
        self.db
            .zs_get(&value_pool_cf, &())
            .unwrap_or_else(ValueBalance::zero)
    }
}

impl DiskWriteBatch {
    // History tree methods

    /// Updates the history tree for the tip, if it is not empty.
    pub fn update_history_tree(&mut self, zebra_db: &ZebraDb, tree: &HistoryTree) {
        let history_tree_cf = zebra_db.db.cf_handle("history_tree").unwrap();

        if let Some(tree) = tree.as_ref().as_ref() {
            self.zs_insert(&history_tree_cf, (), tree);
        }
    }

    /// Legacy method: Deletes the range of history trees at the given [`Height`]s.
    /// Doesn't delete the upper bound.
    ///
    /// From state format 25.3.0 onwards, the history trees are indexed by an empty key,
    /// so this method does nothing.
    pub fn delete_range_history_tree(&mut self, zebra_db: &ZebraDb, from: &Height, to: &Height) {
        let history_tree_cf = zebra_db.db.cf_handle("history_tree").unwrap();

        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        self.zs_delete_range(&history_tree_cf, from, to);
    }

    // Value pool methods

    /// Prepare a database batch containing the chain value pool update from `finalized.block`,
    /// and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating value pools
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_chain_value_pools_batch(
        &mut self,
        db: &DiskDb,
        finalized: &SemanticallyVerifiedBlock,
        utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let tip_chain_value_pool = db.cf_handle("tip_chain_value_pool").unwrap();

        let SemanticallyVerifiedBlock { block, .. } = finalized;

        let new_pool = value_pool.add_block(block.borrow(), &utxos_spent_by_block)?;
        self.zs_insert(&tip_chain_value_pool, (), new_pool);

        Ok(())
    }
}
