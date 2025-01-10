//! Sapling-specific database reading and writing.
//!
//! The sapling scanner database has the following format:
//!
//! | name               | Reading & Writing Key/Values                    |
//! |--------------------|-------------------------------------------------|
//! | [`SAPLING_TX_IDS`] | [`SaplingTxIdsCf`] & [`WriteSaplingTxIdsBatch`] |
//!
//! And types:
//! `SaplingScannedResult`: same as `transaction::Hash`, but with bytes in display order.
//! `None` is stored as a zero-length array of bytes.
//!
//! `SaplingScannedDatabaseIndex` = `SaplingScanningKey` | `TransactionLocation`
//! `TransactionLocation` = `Height` | `TransactionIndex`
//!
//! This format allows us to efficiently find all the results for each key, and the latest height
//! for each key.
//!
//! If there are no results for a height, we store `None` as the result for the coinbase
//! transaction. This allows is to scan each key from the next height after we restart. We also use
//! this mechanism to store key birthday heights, by storing the height before the birthday as the
//! "last scanned" block.

use std::{
    collections::{BTreeMap, HashMap},
    ops::RangeBounds,
};

use itertools::Itertools;

use zebra_chain::block::Height;
use zebra_state::{
    DiskWriteBatch, SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex, SaplingScannedResult,
    SaplingScanningKey, TransactionIndex, TransactionLocation, TypedColumnFamily, WriteTypedBatch,
};

use crate::storage::{Storage, INSERT_CONTROL_INTERVAL};

/// The name of the sapling transaction IDs result column family.
///
/// This constant should be used so the compiler can detect typos.
pub const SAPLING_TX_IDS: &str = "sapling_tx_ids";

/// The type for reading sapling transaction IDs results from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type SaplingTxIdsCf<'cf> =
    TypedColumnFamily<'cf, SaplingScannedDatabaseIndex, Option<SaplingScannedResult>>;

/// The type for writing sapling transaction IDs results from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type WriteSaplingTxIdsBatch<'cf> =
    WriteTypedBatch<'cf, SaplingScannedDatabaseIndex, Option<SaplingScannedResult>, DiskWriteBatch>;

impl Storage {
    // Reading Sapling database entries

    /// Returns the result for a specific database index (key, block height, transaction index).
    /// Returns `None` if the result is missing or an empty marker for a birthday or progress
    /// height.
    pub fn sapling_result_for_index(
        &self,
        index: &SaplingScannedDatabaseIndex,
    ) -> Option<SaplingScannedResult> {
        self.sapling_tx_ids_cf().zs_get(index).flatten()
    }

    /// Returns the results for a specific key and block height.
    pub fn sapling_results_for_key_and_height(
        &self,
        sapling_key: &SaplingScanningKey,
        height: Height,
    ) -> BTreeMap<TransactionIndex, Option<SaplingScannedResult>> {
        let kh_min = SaplingScannedDatabaseIndex::min_for_key_and_height(sapling_key, height);
        let kh_max = SaplingScannedDatabaseIndex::max_for_key_and_height(sapling_key, height);

        self.sapling_results_in_range(kh_min..=kh_max)
            .into_iter()
            .map(|(result_index, txid)| (result_index.tx_loc.index, txid))
            .collect()
    }

    /// Returns all the results for a specific key, indexed by height.
    pub fn sapling_results_for_key(
        &self,
        sapling_key: &SaplingScanningKey,
    ) -> BTreeMap<Height, Vec<SaplingScannedResult>> {
        let k_min = SaplingScannedDatabaseIndex::min_for_key(sapling_key);
        let k_max = SaplingScannedDatabaseIndex::max_for_key(sapling_key);

        // Get an iterator of individual transaction results, and turn it into a HashMap by height
        let results: HashMap<Height, Vec<Option<SaplingScannedResult>>> = self
            .sapling_results_in_range(k_min..=k_max)
            .into_iter()
            .map(|(index, result)| (index.tx_loc.height, result))
            .into_group_map();

        // But we want Vec<SaplingScannedResult>, with empty Vecs instead of [None, None, ...]
        results
            .into_iter()
            .map(|(index, vector)| -> (Height, Vec<SaplingScannedResult>) {
                (index, vector.into_iter().flatten().collect())
            })
            .collect()
    }

    /// Returns all the keys and their last scanned heights.
    pub fn sapling_keys_and_last_scanned_heights(&self) -> HashMap<SaplingScanningKey, Height> {
        let sapling_tx_ids = self.sapling_tx_ids_cf();
        let mut keys = HashMap::new();

        let mut last_stored_record = sapling_tx_ids.zs_last_key_value();

        while let Some((last_stored_record_index, _result)) = last_stored_record {
            let sapling_key = last_stored_record_index.sapling_key.clone();
            let height = last_stored_record_index.tx_loc.height;

            let prev_height = keys.insert(sapling_key.clone(), height);
            assert_eq!(
                prev_height, None,
                "unexpected duplicate key: keys must only be inserted once \
                 last_stored_record_index: {last_stored_record_index:?}",
            );

            // Skip all the results until the next key.
            last_stored_record = sapling_tx_ids.zs_prev_key_value_strictly_before(
                &SaplingScannedDatabaseIndex::min_for_key(&sapling_key),
            );
        }

        keys
    }

    /// Returns the Sapling indexes and results in the supplied range.
    ///
    /// Convenience method for accessing raw data with the correct types.
    fn sapling_results_in_range(
        &self,
        range: impl RangeBounds<SaplingScannedDatabaseIndex>,
    ) -> BTreeMap<SaplingScannedDatabaseIndex, Option<SaplingScannedResult>> {
        self.sapling_tx_ids_cf().zs_items_in_range_ordered(range)
    }

    // Column family convenience methods

    /// Returns a typed handle to the `sapling_tx_ids` column family.
    pub(crate) fn sapling_tx_ids_cf(&self) -> SaplingTxIdsCf {
        SaplingTxIdsCf::new(&self.db, SAPLING_TX_IDS)
            .expect("column family was created when database was created")
    }

    // Writing database entries
    //
    // To avoid exposing internal types, and accidentally forgetting to write a batch,
    // each pub(crate) write method should write an entire batch.

    /// Inserts a batch of scanned sapling result for a key and height.
    /// If a result already exists for that key, height, and index, it is replaced.
    pub fn insert_sapling_results(
        &mut self,
        sapling_key: &SaplingScanningKey,
        height: Height,
        sapling_results: BTreeMap<TransactionIndex, SaplingScannedResult>,
    ) {
        // We skip key heights that have one or more results, so the results for each key height
        // must be in a single batch.
        let mut batch = self.sapling_tx_ids_cf().new_batch_for_writing();

        // Every `INSERT_CONTROL_INTERVAL` we add a new entry to the scanner database for each key
        // so we can track progress made in the last interval even if no transaction was yet found.
        let needs_control_entry =
            height.0 % INSERT_CONTROL_INTERVAL == 0 && sapling_results.is_empty();

        // Add scanner progress tracking entry for key.
        // Defensive programming: add the tracking entry first, so that we don't accidentally
        // overwrite real results with it. (This is currently prevented by the empty check.)
        if needs_control_entry {
            batch = batch.insert_sapling_height(sapling_key, height);
        }

        for (index, sapling_result) in sapling_results {
            let index = SaplingScannedDatabaseIndex {
                sapling_key: sapling_key.clone(),
                tx_loc: TransactionLocation::from_parts(height, index),
            };

            let entry = SaplingScannedDatabaseEntry {
                index,
                value: Some(sapling_result),
            };

            batch = batch.zs_insert(&entry.index, &entry.value);
        }

        batch
            .write_batch()
            .expect("unexpected database write failure");
    }

    /// Insert a sapling scanning `key`, and mark all heights before `birthday_height` so they
    /// won't be scanned.
    ///
    /// If a result already exists for the coinbase transaction at the height before the birthday,
    /// it is replaced with an empty result. This can happen if the user increases the birthday
    /// height.
    ///
    /// TODO: ignore incorrect changes to birthday heights
    pub(crate) fn insert_sapling_key(
        &mut self,
        sapling_key: &SaplingScanningKey,
        birthday_height: Option<Height>,
    ) {
        let min_birthday_height = self.network().sapling_activation_height();

        // The birthday height must be at least the minimum height for that pool.
        let birthday_height = birthday_height
            .unwrap_or(min_birthday_height)
            .max(min_birthday_height);
        // And we want to skip up to the height before it.
        let skip_up_to_height = birthday_height.previous().unwrap_or(Height::MIN);

        // It's ok to write some keys and not others during shutdown, so each key can get its own
        // batch. (They will be re-written on startup anyway.)
        //
        // TODO: ignore incorrect changes to birthday heights,
        //       and redundant birthday heights
        self.sapling_tx_ids_cf()
            .new_batch_for_writing()
            .insert_sapling_height(sapling_key, skip_up_to_height)
            .write_batch()
            .expect("unexpected database write failure");
    }

    /// Delete the sapling keys and their results, if they exist,
    pub fn delete_sapling_keys(&mut self, keys: Vec<SaplingScanningKey>) {
        self.sapling_tx_ids_cf()
            .new_batch_for_writing()
            .delete_sapling_keys(keys)
            .write_batch()
            .expect("unexpected database write failure");
    }

    /// Delete the results of sapling scanning `keys`, if they exist
    pub(crate) fn delete_sapling_results(&mut self, keys: Vec<SaplingScanningKey>) {
        let mut batch = self
            .sapling_tx_ids_cf()
            .new_batch_for_writing()
            .delete_sapling_keys(keys.clone());

        for key in &keys {
            batch = batch.insert_sapling_height(key, Height::MIN);
        }

        batch
            .write_batch()
            .expect("unexpected database write failure");
    }
}

/// Utility trait for inserting sapling heights into a WriteSaplingTxIdsBatch.
trait InsertSaplingHeight {
    fn insert_sapling_height(self, sapling_key: &SaplingScanningKey, height: Height) -> Self;
}

impl InsertSaplingHeight for WriteSaplingTxIdsBatch<'_> {
    /// Insert sapling height with no results.
    ///
    /// If a result already exists for the coinbase transaction at that height,
    /// it is replaced with an empty result. This should never happen.
    fn insert_sapling_height(self, sapling_key: &SaplingScanningKey, height: Height) -> Self {
        let index = SaplingScannedDatabaseIndex::min_for_key_and_height(sapling_key, height);

        // TODO: assert that we don't overwrite any entries here.
        self.zs_insert(&index, &None)
    }
}

/// Utility trait for deleting sapling keys in a WriteSaplingTxIdsBatch.
trait DeleteSaplingKeys {
    fn delete_sapling_keys(self, sapling_key: Vec<SaplingScanningKey>) -> Self;
}

impl DeleteSaplingKeys for WriteSaplingTxIdsBatch<'_> {
    /// Delete sapling keys and their results.
    fn delete_sapling_keys(mut self, sapling_keys: Vec<SaplingScanningKey>) -> Self {
        for key in &sapling_keys {
            let from_index = SaplingScannedDatabaseIndex::min_for_key(key);
            let until_strictly_before_index = SaplingScannedDatabaseIndex::max_for_key(key);

            self = self
                .zs_delete_range(&from_index, &until_strictly_before_index)
                // TODO: convert zs_delete_range() to take std::ops::RangeBounds
                .zs_delete(&until_strictly_before_index);
        }

        self
    }
}
