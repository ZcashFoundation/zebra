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
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use zebra_chain::{
    amount::NonNegative,
    block::Height,
    block_info::BlockInfo,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::NetworkUpgrade,
    primitives::zcash_history::{Entry, HistoryNodeIndex},
    serialization::ZcashSerialize as _,
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::DiskWriteBatch,
        disk_format::{chain::HistoryTreeParts, RawBytes},
        zebra_db::ZebraDb,
        TypedColumnFamily,
    },
    BoxError, HashOrHeight,
};

/// The name of the History Tree column family.
///
/// This constant should be used so the compiler can detect typos.
pub const HISTORY_TREE: &str = "history_tree";

/// The type for reading history trees from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type HistoryTreePartsCf<'cf> = TypedColumnFamily<'cf, (), HistoryTreeParts>;

/// The legacy (1.3.0 and earlier) type for reading history trees from the database.
/// This type should not be used in new code.
pub type LegacyHistoryTreePartsCf<'cf> = TypedColumnFamily<'cf, Height, HistoryTreeParts>;

/// A generic raw key type for reading history trees from the database, regardless of the database version.
/// This type should not be used in new code.
pub type RawHistoryTreePartsCf<'cf> = TypedColumnFamily<'cf, RawBytes, HistoryTreeParts>;

/// The name of the history node column family.
///
/// This constant should be used so the compiler can detect typos.
pub const HISTORY_NODE: &str = "history_node";

/// The type for reading history nodes from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type HistoryNodeCf<'cf> = TypedColumnFamily<'cf, HistoryNodeIndex, Entry>;

/// The name of the tip-only chain value pools column family.
///
/// This constant should be used so the compiler can detect typos.
pub const CHAIN_VALUE_POOLS: &str = "tip_chain_value_pool";

/// The type for reading value pools from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type ChainValuePoolsCf<'cf> = TypedColumnFamily<'cf, (), ValueBalance<NonNegative>>;

/// The name of the block info column family.
///
/// This constant should be used so the compiler can detect typos.
pub const BLOCK_INFO: &str = "block_info";

/// The type for reading value pools from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type BlockInfoCf<'cf> = TypedColumnFamily<'cf, Height, BlockInfo>;

impl ZebraDb {
    // Column family convenience methods

    /// Returns a typed handle to the `history_tree` column family.
    pub(crate) fn history_tree_cf(&self) -> HistoryTreePartsCf<'_> {
        HistoryTreePartsCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    /// Returns a legacy typed handle to the `history_tree` column family.
    /// This should not be used in new code.
    pub(crate) fn legacy_history_tree_cf(&self) -> LegacyHistoryTreePartsCf<'_> {
        LegacyHistoryTreePartsCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    /// Returns a generic raw key typed handle to the `history_tree` column family.
    /// This should not be used in new code.
    pub(crate) fn raw_history_tree_cf(&self) -> RawHistoryTreePartsCf<'_> {
        RawHistoryTreePartsCf::new(&self.db, HISTORY_TREE)
            .expect("column family was created when database was created")
    }

    pub(crate) fn history_node_cf(&self) -> HistoryNodeCf {
        HistoryNodeCf::new(&self.db, HISTORY_NODE)
            .expect("column family was created when database was created")
    }

    /// Returns a typed handle to the chain value pools column family.
    pub(crate) fn chain_value_pools_cf(&self) -> ChainValuePoolsCf<'_> {
        ChainValuePoolsCf::new(&self.db, CHAIN_VALUE_POOLS)
            .expect("column family was created when database was created")
    }

    /// Returns a typed handle to the block data column family.
    pub(crate) fn block_info_cf(&self) -> BlockInfoCf<'_> {
        BlockInfoCf::new(&self.db, BLOCK_INFO)
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
        let mut history_tree_parts = history_tree_cf.zs_get(&());

        if history_tree_parts.is_none() {
            let legacy_history_tree_cf = self.legacy_history_tree_cf();

            // In Zebra 1.4.0 and later, we only update the history tip tree when it has changed (for every block after heartwood).
            // But we write with a `()` key, not a height key.
            // So we need to look for the most recent update height if the `()` key has never been written.
            history_tree_parts = legacy_history_tree_cf
                .zs_last_key_value()
                .map(|(_height_key, tree_value)| tree_value);
        }

        let history_tree = history_tree_parts.map(|parts| {
            parts.with_network(&self.db.network()).expect(
                "deserialization format should match the serialization format used by IntoDisk",
            )
        });
        Arc::new(HistoryTree::from(history_tree))
    }

    /// Returns the ZIP-221 history tree at the given height.
    ///
    /// Returns `None` if history trees had not been activated at that height (pre-Heartwood),
    /// if the height is larger than the finalized tip height, or if the state is empty.
    pub fn history_tree_by_height(&self, height: Height) -> Option<Arc<HistoryTree>> {
        // There is no history tree before heartwood activation
        let heartwood_activation = NetworkUpgrade::Heartwood.activation_height(&self.network())?;
        if height <= heartwood_activation || height > self.finalized_tip_height()? {
            return None;
        }

        // Get network and upgrade from height.
        // The upgrade is the previous one if this is an activation height.
        let network = self.db.network();
        let mut temp_tree_height = height;
        let mut upgrade = NetworkUpgrade::current(&network, height);
        let activation_height = upgrade.activation_height(&self.network())?;
        if activation_height == height {
            upgrade = upgrade.previous_upgrade()?;
            temp_tree_height = (temp_tree_height - 1)?
        }

        // Create temporary tree from block at height.
        // Use the previous one if we are at an activation height.
        let block = self.block(HashOrHeight::Height(
            Height::try_from(temp_tree_height.as_usize() as u32).ok()?,
        ))?;
        let sapling_root = self
            .sapling_tree_by_height(&Height::try_from(temp_tree_height.as_usize() as u32).ok()?)?
            .root();
        let orchard_root = self
            .orchard_tree_by_height(&Height::try_from(temp_tree_height.as_usize() as u32).ok()?)?
            .root();
        let temp_tree =
            HistoryTree::from_block(&network, block, &sapling_root, &orchard_root).ok()?;

        // Get the peaks at the last block height covered by the tree,
        // which is that of the previous block.
        let peak_ids = temp_tree.peaks_at((height - 1)?)?;

        // Read the peaks and build the inner tree
        let mut peaks = BTreeMap::<u32, Entry>::new();
        for i in peak_ids {
            let index = HistoryNodeIndex { upgrade, index: i };
            peaks.insert(i, self.history_node(index)?);
        }

        let inner_tree = NonEmptyHistoryTree::from_cache(
            &network,
            temp_tree.node_count_at(height)?,
            peaks,
            (height - 1)?,
        );

        // Return the outer tree
        match inner_tree {
            Ok(tree) => Some(Arc::new(HistoryTree::from(tree))),
            Err(_) => None,
        }
    }

    /// Returns the ZIP-221 history tree at the last block of the given network upgrade.
    ///
    /// Returns `None` if history trees had not been activated at that height (pre-Heartwood),
    /// if the update does not exist in this chain, or if the state is empty.
    pub fn history_tree_by_upgrade(&self, upgrade: NetworkUpgrade) -> Option<Arc<HistoryTree>> {
        let history_tree_upgrades: Vec<NetworkUpgrade> = NetworkUpgrade::iter()
            .skip_while(|u| *u != NetworkUpgrade::Heartwood)
            .take_while(|u| *u != NetworkUpgrade::Nu6)
            .chain(std::iter::once(NetworkUpgrade::Nu6))
            .collect();

        if history_tree_upgrades.contains(&upgrade) {
            // Get the last block height of this upgrade, or the tip height if this is the last upgrade
            let last_height = upgrade.next_upgrade().map_or_else(
                || self.finalized_tip_height(),
                |next| {
                    next.activation_height(&self.network())
                        .map_or_else(|| self.finalized_tip_height(), |height| height - 1)
                },
            )?;

            self.history_tree_by_height(last_height)
        } else {
            None
        }
    }

    /// Returns all the history tip trees.
    /// We only store the history tree for the tip, so this method is only used in tests and
    /// upgrades.
    pub(crate) fn history_trees_full_tip(&self) -> BTreeMap<RawBytes, Arc<HistoryTree>> {
        let raw_history_tree_cf = self.raw_history_tree_cf();

        raw_history_tree_cf
            .zs_forward_range_iter(..)
            .map(|(raw_key, history_tree_parts)| {
                let history_tree = history_tree_parts.with_network(&self.db.network()).expect(
                    "deserialization format should match the serialization format used by IntoDisk",
                );
                (raw_key, Arc::new(HistoryTree::from(history_tree)))
            })
            .collect()
    }

    /// Returns the history node at the given index.
    pub fn history_node(&self, index: HistoryNodeIndex) -> Option<Entry> {
        self.history_node_cf().zs_get(&index)
    }

    /// Returns the last history node index.                                                                                            
    pub fn last_history_node_index(&self) -> Option<HistoryNodeIndex> {
        self.history_node_cf()
            .zs_last_key_value()
            .map(|(index, _)| index)
    }

    /// Returns the last history node.
    pub fn last_history_node(&self) -> Option<Entry> {
        self.history_node_cf()
            .zs_last_key_value()
            .map(|(_, entry)| entry)
    }

    // Value pool methods

    /// Returns the stored `ValueBalance` for the best chain at the finalized tip height.
    pub fn finalized_value_pool(&self) -> ValueBalance<NonNegative> {
        let chain_value_pools_cf = self.chain_value_pools_cf();

        chain_value_pools_cf
            .zs_get(&())
            .unwrap_or_else(ValueBalance::zero)
    }

    /// Returns the stored `BlockInfo` for the given block.
    pub fn block_info(&self, hash_or_height: HashOrHeight) -> Option<BlockInfo> {
        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;

        let block_info_cf = self.block_info_cf();

        block_info_cf.zs_get(&height)
    }
}

impl DiskWriteBatch {
    // History tree methods

    /// Updates the history tree for the tip, if it is not empty.
    ///
    /// The batch must be written to the database by the caller.
    pub fn update_history_tree(&mut self, db: &ZebraDb, tree: &HistoryTree) {
        let history_tree_cf = db.history_tree_cf().with_batch_for_writing(self);

        if let Some(tree) = tree.as_ref() {
            // The batch is modified by this method and written by the caller.
            let _ = history_tree_cf.zs_insert(&(), &HistoryTreeParts::from(tree));
        }
    }

    /// Writes a history tree node to the database.
    ///
    /// The batch must be written to the database by the caller.
    pub fn write_history_node(&mut self, db: &ZebraDb, index: HistoryNodeIndex, node: Entry) {
        let history_node_cf = db.history_node_cf().with_batch_for_writing(self);

        let _ = history_node_cf.zs_insert(&index, &node);
    }

    /// Removes all history nodes from the database.
    ///
    /// Should only be used for debug and database upgrade.
    pub fn clear_history_nodes(&mut self, db: &ZebraDb) {
        let history_node_cf = db.history_node_cf().with_batch_for_writing(self);

        let from = HistoryNodeIndex {
            upgrade: zebra_chain::parameters::NetworkUpgrade::Heartwood,
            index: 0,
        };
        let until_strictly_before = HistoryNodeIndex {
            upgrade: zebra_chain::parameters::NetworkUpgrade::Nu6,
            index: u32::MAX,
        };
        let _ = history_node_cf.zs_delete_range(&from, &until_strictly_before);
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

    /// Prepares a database batch containing the chain value pool update from `finalized.block`, and
    /// returns it without actually writing anything.
    ///
    /// The batch is modified by this method and written by the caller. The caller should not write
    /// the batch if this method returns an error.
    ///
    /// The parameter `utxos_spent_by_block` must contain the [`transparent::Utxo`]s of every input
    /// in this block, including UTXOs created by earlier transactions in this block.
    ///
    /// Note that the chain value pool has the opposite sign to the transaction value pool. See the
    /// [`chain_value_pool_change`] and [`add_chain_value_pool_change`] methods for more details.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating value pools
    ///
    /// [`chain_value_pool_change`]: zebra_chain::block::Block::chain_value_pool_change
    /// [`add_chain_value_pool_change`]: ValueBalance::add_chain_value_pool_change
    pub fn prepare_chain_value_pools_batch(
        &mut self,
        db: &ZebraDb,
        finalized: &FinalizedBlock,
        utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let new_value_pool =
            value_pool.add_chain_value_pool_change(finalized.block.chain_value_pool_change(
                &utxos_spent_by_block,
                finalized.deferred_pool_balance_change,
            )?)?;
        let _ = db
            .chain_value_pools_cf()
            .with_batch_for_writing(self)
            .zs_insert(&(), &new_value_pool);

        // Get the block size to store with the BlockInfo. This is a bit wasteful
        // since the block header and txs were serialized previously when writing
        // them to the DB, and we could get the size if we modified the database
        // code to return the size of data written; but serialization should be cheap.
        let block_size = finalized.block.zcash_serialized_size();

        let _ = db.block_info_cf().with_batch_for_writing(self).zs_insert(
            &finalized.height,
            &BlockInfo::new(new_value_pool, block_size as u32),
        );

        Ok(())
    }
}
