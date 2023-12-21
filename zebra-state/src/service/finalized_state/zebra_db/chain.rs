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
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use zebra_chain::{
    amount::NonNegative,
    block::Height,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::DiskWriteBatch, disk_format::RawBytes, zebra_db::ZebraDb, TypedColumnFamily,
    },
    BoxError,
};

/// The name of the History Tree column family.
///
/// This constant should be used so the compiler can detect typos.
pub const HISTORY_TREE: &str = "history_tree";

/// The type for reading history trees from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type HistoryTreeCf<'cf> = TypedColumnFamily<'cf, (), NonEmptyHistoryTree>;

/// The legacy (1.3.0 and earlier) type for reading history trees from the database.
/// This type should not be used in new code.
pub type LegacyHistoryTreeCf<'cf> = TypedColumnFamily<'cf, Height, NonEmptyHistoryTree>;

/// A generic raw key type for reading history trees from the database, regardless of the database version.
/// This type should not be used in new code.
pub type RawHistoryTreeCf<'cf> = TypedColumnFamily<'cf, RawBytes, NonEmptyHistoryTree>;

/// The name of the chain value pools column family.
///
/// This constant should be used so the compiler can detect typos.
pub const CHAIN_VALUE_POOLS: &str = "tip_chain_value_pool";

/// The type for reading value pools from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type ChainValuePoolsCf<'cf> = TypedColumnFamily<'cf, (), ValueBalance<NonNegative>>;

impl ZebraDb {
    // Column family convenience methods

    /// Returns a typed handle to the `history_tree` column family.
    pub(crate) fn history_tree_cf(&self) -> HistoryTreeCf {
        HistoryTreeCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    /// Returns a legacy typed handle to the `history_tree` column family.
    /// This should not be used in new code.
    pub(crate) fn legacy_history_tree_cf(&self) -> LegacyHistoryTreeCf {
        LegacyHistoryTreeCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    /// Returns a generic raw key typed handle to the `history_tree` column family.
    /// This should not be used in new code.
    pub(crate) fn raw_history_tree_cf(&self) -> RawHistoryTreeCf {
        RawHistoryTreeCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    /// Returns a typed handle to the chain value pools column family.
    pub(crate) fn chain_value_pools_cf(&self) -> ChainValuePoolsCf {
        ChainValuePoolsCf::new(&self.db, CHAIN_VALUE_POOLS)
            .expect("column family was created when database was created")
    }

    // History tree methods

    /// Returns the ZIP-221 history tree of the finalized tip.
    ///
    /// If history trees have not been activated yet (pre-Heartwood), or the state is empty,
    /// returns an empty history tree.
    pub fn history_tree(&self) -> Arc<HistoryTree> {
        let history_tree_cf = self.history_tree_cf();

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
        let mut history_tree = history_tree_cf.zs_get(&());

        if history_tree.is_none() {
            let legacy_history_tree_cf = self.legacy_history_tree_cf();

            // In Zebra 1.4.0 and later, we only update the history tip tree when it has changed (for every block after heartwood).
            // But we write with a `()` key, not a height key.
            // So we need to look for the most recent update height if the `()` key has never been written.
            history_tree = legacy_history_tree_cf
                .zs_last_key_value()
                .map(|(_height_key, tree_value)| tree_value);
        }

        Arc::new(HistoryTree::from(history_tree))
    }

    /// Returns all the history tip trees.
    /// We only store the history tree for the tip, so this method is only used in tests and
    /// upgrades.
    pub(crate) fn history_trees_full_tip(&self) -> BTreeMap<RawBytes, Arc<HistoryTree>> {
        let raw_history_tree_cf = self.raw_history_tree_cf();

        raw_history_tree_cf
            .zs_forward_range_iter(..)
            .map(|(raw_key, history_tree)| (raw_key, Arc::new(HistoryTree::from(history_tree))))
            .collect()
    }

    // Value pool methods

    /// Returns the stored `ValueBalance` for the best chain at the finalized tip height.
    pub fn finalized_value_pool(&self) -> ValueBalance<NonNegative> {
        let chain_value_pools_cf = self.chain_value_pools_cf();

        chain_value_pools_cf
            .zs_get(&())
            .unwrap_or_else(ValueBalance::zero)
    }
}

impl DiskWriteBatch {
    // History tree methods

    /// Updates the history tree for the tip, if it is not empty.
    ///
    /// The batch must be written to the database by the caller.
    pub fn update_history_tree(&mut self, db: &ZebraDb, tree: &HistoryTree) {
        let history_tree_cf = db.history_tree_cf().with_batch_for_writing(self);

        if let Some(tree) = tree.as_ref().as_ref() {
            // The batch is modified by this method and written by the caller.
            let _ = history_tree_cf.zs_insert(&(), tree);
        }
    }

    /// Legacy method: Deletes the range of history trees at the given [`Height`]s.
    /// Doesn't delete the upper bound.
    ///
    /// From state format 25.3.0 onwards, the history trees are indexed by an empty key,
    /// so this method does nothing.
    ///
    /// The batch must be written to the database by the caller.
    pub fn delete_range_history_tree(
        &mut self,
        db: &ZebraDb,
        from: &Height,
        until_strictly_before: &Height,
    ) {
        let history_tree_cf = db.legacy_history_tree_cf().with_batch_for_writing(self);

        // The batch is modified by this method and written by the caller.
        //
        // TODO: convert zs_delete_range() to take std::ops::RangeBounds
        let _ = history_tree_cf.zs_delete_range(from, until_strictly_before);
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
        db: &ZebraDb,
        finalized: &FinalizedBlock,
        utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let chain_value_pools_cf = db.chain_value_pools_cf().with_batch_for_writing(self);

        let FinalizedBlock { block, .. } = finalized;

        let new_pool = value_pool.add_block(block.borrow(), &utxos_spent_by_block)?;

        // The batch is modified by this method and written by the caller.
        let _ = chain_value_pools_cf.zs_insert(&(), &new_pool);

        Ok(())
    }
}
