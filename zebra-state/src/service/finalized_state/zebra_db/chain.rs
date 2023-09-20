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
    amount::NonNegative,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    request::SemanticallyVerifiedBlockWithTrees,
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::RawBytes,
        zebra_db::ZebraDb,
    },
    BoxError, SemanticallyVerifiedBlock,
};

impl ZebraDb {
    /// Returns the ZIP-221 history tree of the finalized tip or `None`
    /// if it does not exist yet in the state (pre-Heartwood).
    pub fn history_tree(&self) -> Arc<HistoryTree> {
        if self.finalized_tip_height().is_some() {
            let history_tree_cf = self.db.cf_handle("history_tree").unwrap();

            // # Concurrency
            //
            // There is only one tree in this column family, which is atomically updated by a block
            // write batch (database transaction). If this update runs between the height read and
            // the tree read, the height will be wrong, and the tree will be missing.
            // That could cause consensus bugs.
            //
            // Instead, always read the last tree in the column family, regardless of height.
            //
            // See ticket #7581 for more details.
            //
            // TODO: this concurrency bug will be permanently fixed in PR #7392,
            //       by changing the block update to overwrite the tree, rather than deleting it.
            //
            // # Forwards Compatibility
            //
            // This code can read the column family format in 1.2.0 and earlier (tip height key),
            // and after PR #7392 is merged (empty key).
            let history_tree: Option<NonEmptyHistoryTree> = self
                .db
                .zs_last_key_value(&history_tree_cf)
                // RawBytes will deserialize both Height and `()` (empty) keys.
                .map(|(_key, value): (RawBytes, _)| value);

            if let Some(non_empty_tree) = history_tree {
                return Arc::new(HistoryTree::from(non_empty_tree));
            }
        }

        Default::default()
    }

    /// Returns the stored `ValueBalance` for the best chain at the finalized tip height.
    pub fn finalized_value_pool(&self) -> ValueBalance<NonNegative> {
        let value_pool_cf = self.db.cf_handle("tip_chain_value_pool").unwrap();
        self.db
            .zs_get(&value_pool_cf, &())
            .unwrap_or_else(ValueBalance::zero)
    }
}

impl DiskWriteBatch {
    /// Prepare a database batch containing the history tree updates
    /// from `finalized.block`, and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Returns any errors from updating the history tree
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_history_batch(
        &mut self,
        db: &DiskDb,
        finalized: &SemanticallyVerifiedBlockWithTrees,
    ) -> Result<(), BoxError> {
        let history_tree_cf = db.cf_handle("history_tree").unwrap();

        let height = finalized.verified.height;

        // Update the tree in state
        let current_tip_height = height - 1;
        if let Some(h) = current_tip_height {
            self.zs_delete(&history_tree_cf, h);
        }

        // TODO: if we ever need concurrent read-only access to the history tree,
        // store it by `()`, not height.
        // Otherwise, the ReadStateService could access a height
        // that was just deleted by a concurrent StateService write.
        // This requires a database version update.
        if let Some(history_tree) = finalized.treestate.history_tree.as_ref().as_ref() {
            self.zs_insert(&history_tree_cf, height, history_tree);
        }

        Ok(())
    }

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
