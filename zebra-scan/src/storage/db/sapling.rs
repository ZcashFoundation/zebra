//! Sapling-specific database reading and writing.
//!
//! The sapling scanner database has the following format:
//!
//! | name             | key                           | value                    |
//! |------------------|-------------------------------|--------------------------|
//! | `sapling_tx_ids` | `SaplingScannedDatabaseIndex` | `Option<SaplingScannedResult>`      |
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
    AsColumnFamilyRef, ReadDisk, SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex,
    SaplingScannedResult, SaplingScanningKey, TransactionIndex, WriteDisk,
};

use crate::storage::Storage;

use super::ScannerWriteBatch;

/// The name of the sapling transaction IDs result column family.
///
/// This constant should be used so the compiler can detect typos.
pub const SAPLING_TX_IDS: &str = "sapling_tx_ids";

impl Storage {
    // Reading Sapling database entries

    /// Returns the result for a specific database index (key, block height, transaction index).
    /// Returns `None` if the result is missing or an empty marker for a birthday or progress
    /// height.
    //
    // TODO: add tests for this method
    pub fn sapling_result_for_index(
        &self,
        index: &SaplingScannedDatabaseIndex,
    ) -> Option<SaplingScannedResult> {
        self.db.zs_get(&self.sapling_tx_ids_cf(), &index).flatten()
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

        tracing::info!("searching backwards for last entry");
        let mut last_stored_record: Option<(
            SaplingScannedDatabaseIndex,
            Option<SaplingScannedResult>,
        )> = self.db.zs_last_key_value(&sapling_tx_ids);

        loop {
            let Some((mut last_stored_record_index, _result)) = last_stored_record else {
                return keys;
            };

            let sapling_key = last_stored_record_index.sapling_key.clone();
            let height = last_stored_record_index.tx_loc.height;

            tracing::info!(?sapling_key, ?height, "inserting key and last height");
            let prev_height = keys.insert(sapling_key.clone(), height);
            assert_eq!(
                prev_height, None,
                "unexpected duplicate key: keys must only be inserted once\
                 last_stored_record_index: {last_stored_record_index:?}",
            );

            // Skip all the results until the next key.
            last_stored_record_index = SaplingScannedDatabaseIndex::min_for_key(&sapling_key);
            tracing::info!(
                ?last_stored_record_index,
                "searching backwards strictly before"
            );
            last_stored_record = self
                .db
                .zs_prev_key_value_strictly_before(&sapling_tx_ids, &last_stored_record_index);
            tracing::info!(?last_stored_record_index, "found last storede");
        }
    }

    /// Returns the Sapling indexes and results in the supplied range.
    ///
    /// Convenience method for accessing raw data with the correct types.
    fn sapling_results_in_range(
        &self,
        range: impl RangeBounds<SaplingScannedDatabaseIndex>,
    ) -> BTreeMap<SaplingScannedDatabaseIndex, Option<SaplingScannedResult>> {
        self.db
            .zs_items_in_range_ordered(&self.sapling_tx_ids_cf(), range)
    }

    // Column family convenience methods

    /// Returns a handle to the `sapling_tx_ids` column family.
    pub(crate) fn sapling_tx_ids_cf(&self) -> impl AsColumnFamilyRef + '_ {
        self.db
            .cf_handle(SAPLING_TX_IDS)
            .expect("column family was created when database was created")
    }

    // Writing batches

    /// Write `batch` to the database for this storage.
    pub(crate) fn write_batch(&self, batch: ScannerWriteBatch) {
        // Just panic on errors for now.
        self.db
            .write_batch(batch.0)
            .expect("unexpected database error")
    }
}

// Writing database entries
//
// TODO: split the write type into state and scanner, so we can't call state write methods on
// scanner databases
impl ScannerWriteBatch {
    /// Inserts a scanned sapling result for a key and height.
    /// If a result already exists for that key and height, it is replaced.
    pub(crate) fn insert_sapling_result(
        &mut self,
        storage: &Storage,
        entry: SaplingScannedDatabaseEntry,
    ) {
        self.zs_insert(&storage.sapling_tx_ids_cf(), entry.index, entry.value);
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
        storage: &Storage,
        sapling_key: &SaplingScanningKey,
        birthday_height: Option<Height>,
    ) {
        let min_birthday_height = storage.min_sapling_birthday_height();

        // The birthday height must be at least the minimum height for that pool.
        let birthday_height = birthday_height
            .unwrap_or(min_birthday_height)
            .max(min_birthday_height);
        // And we want to skip up to the height before it.
        let skip_up_to_height = birthday_height.previous().unwrap_or(Height::MIN);

        let index =
            SaplingScannedDatabaseIndex::min_for_key_and_height(sapling_key, skip_up_to_height);
        self.zs_insert(&storage.sapling_tx_ids_cf(), index, None);
    }

    /// Insert sapling height with no results
    pub(crate) fn insert_sapling_height(
        &mut self,
        storage: &Storage,
        sapling_key: &SaplingScanningKey,
        height: Height,
    ) {
        let index = SaplingScannedDatabaseIndex::min_for_key_and_height(sapling_key, height);
        self.zs_insert(&storage.sapling_tx_ids_cf(), index, None);
    }
}
