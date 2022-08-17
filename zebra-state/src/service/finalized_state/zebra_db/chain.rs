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
    orchard, sapling, transparent,
    value_balance::ValueBalance,
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
    /// Returns the ZIP-221 history tree of the finalized tip or `None`
    /// if it does not exist yet in the state (pre-Heartwood).
    pub fn history_tree(&self) -> Arc<HistoryTree> {
        if let Some(height) = self.finalized_tip_height() {
            let history_tree_cf = self.db.cf_handle("history_tree").unwrap();

            let history_tree: Option<NonEmptyHistoryTree> =
                self.db.zs_get(&history_tree_cf, &height);

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
        finalized: &FinalizedBlock,
        sapling_root: sapling::tree::Root,
        orchard_root: orchard::tree::Root,
        mut history_tree: Arc<HistoryTree>,
    ) -> Result<(), BoxError> {
        let history_tree_cf = db.cf_handle("history_tree").unwrap();

        let FinalizedBlock { block, height, .. } = finalized;

        // TODO: run this CPU-intensive cryptography in a parallel rayon thread, if it shows up in profiles
        let history_tree_mut = Arc::make_mut(&mut history_tree);
        history_tree_mut.push(self.network(), block.clone(), sapling_root, orchard_root)?;

        // Update the tree in state
        let current_tip_height = *height - 1;
        if let Some(h) = current_tip_height {
            self.zs_delete(&history_tree_cf, h);
        }

        // TODO: if we ever need concurrent read-only access to the history tree,
        // store it by `()`, not height.
        // Otherwise, the ReadStateService could access a height
        // that was just deleted by a concurrent StateService write.
        // This requires a database version update.
        if let Some(history_tree) = history_tree.as_ref().as_ref() {
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
        finalized: &FinalizedBlock,
        utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let tip_chain_value_pool = db.cf_handle("tip_chain_value_pool").unwrap();

        let FinalizedBlock { block, .. } = finalized;

        let new_pool = value_pool.add_block(block.borrow(), &utxos_spent_by_block)?;
        self.zs_insert(&tip_chain_value_pool, (), new_pool);

        Ok(())
    }
}
