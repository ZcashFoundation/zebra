//! Sapling-specific database reading and writing.
//!
//! The sapling scanner database has the following format:
//!
//! | name             | key                           | value                    |
//! |------------------|-------------------------------|--------------------------|
//! | `sapling_tx_ids` | `SaplingScannedDatabaseIndex` | `Vec<transaction::Hash>` |
//!
//! And types:
//! SaplingScannedDatabaseIndex = `SaplingScanningKey` | `Height`
//!
//! This format allows us to efficiently find all the results for each key, and the latest height
//! for each key.
//!
//! If there are no results for a height, we store an empty list of results. This allows is to scan
//! each key from the next height after we restart. We also use this mechanism to store key
//! birthday heights, by storing the height before the birthday as the "last scanned" block.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::block::Height;
use zebra_state::{
    AsColumnFamilyRef, ReadDisk, SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex,
    SaplingScannedResult, SaplingScanningKey, WriteDisk,
};

use crate::storage::Storage;

use super::ScannerWriteBatch;

/// The name of the sapling transaction IDs result column family.
///
/// This constant should be used so the compiler can detect typos.
pub const SAPLING_TX_IDS: &str = "sapling_tx_ids";

impl Storage {
    // Reading Sapling database entries

    /// Returns the results for a specific key and block height.
    //
    // TODO: add tests for this method
    pub fn sapling_result_for_key_and_block(
        &self,
        index: &SaplingScannedDatabaseIndex,
    ) -> Vec<SaplingScannedResult> {
        self.db
            .zs_get(&self.sapling_tx_ids_cf(), &index)
            .unwrap_or_default()
    }

    /// Returns all the results for a specific key, indexed by height.
    pub fn sapling_results_for_key(
        &self,
        sapling_key: &SaplingScanningKey,
    ) -> BTreeMap<Height, Vec<SaplingScannedResult>> {
        let k_min = SaplingScannedDatabaseIndex::min_for_key(sapling_key);
        let k_max = SaplingScannedDatabaseIndex::max_for_key(sapling_key);

        self.db
            .zs_items_in_range_ordered(&self.sapling_tx_ids_cf(), k_min..=k_max)
            .into_iter()
            .map(|(index, result)| (index.height, result))
            .collect()
    }

    /// Returns all the keys and their birthday heights.
    pub fn sapling_keys_and_birthday_heights(&self) -> HashMap<SaplingScanningKey, Height> {
        // This code is a bit complex because we don't have a separate column family for keys
        // and their birthday heights.
        //
        // TODO: make a separate column family after the MVP.

        let sapling_tx_ids = self.sapling_tx_ids_cf();
        let mut keys = HashMap::new();

        // The minimum key is invalid or a dummy key, so we will never have an entry for it.
        let mut find_next_key_index = SaplingScannedDatabaseIndex::min();

        loop {
            // Find the next key, and the first height we have for it.
            let Some(entry) = self
                .db
                .zs_next_key_value_from(&sapling_tx_ids, &find_next_key_index)
            else {
                break;
            };

            let (index, results): (_, Vec<SaplingScannedResult>) = entry;
            let SaplingScannedDatabaseIndex {
                sapling_key,
                mut height,
            } = index;

            // If there are no results, then it's a "skip up to height" marker, and the birthday
            // height is the next height. If there are some results, it's the actual birthday
            // height.
            if results.is_empty() {
                height = height
                    .next()
                    .expect("results should only be stored for validated block heights");
            }

            keys.insert(sapling_key.clone(), height);

            // Skip all the results before the next key.
            find_next_key_index = SaplingScannedDatabaseIndex::max_for_key(&sapling_key);
        }

        keys
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
        // Just panic on errors for now
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
    /// If a result already exists for the height before the birthday, it is replaced with an empty
    /// result.
    pub(crate) fn insert_sapling_key(
        &mut self,
        storage: &Storage,
        sapling_key: SaplingScanningKey,
        birthday_height: Option<Height>,
    ) {
        let min_birthday_height = storage.min_sapling_birthday_height();

        // The birthday height must be at least the minimum height for that pool.
        let birthday_height = birthday_height
            .unwrap_or(min_birthday_height)
            .max(min_birthday_height);
        // And we want to skip up to the height before it.
        let skip_up_to_height = birthday_height.previous().unwrap_or(Height(0));

        let index = SaplingScannedDatabaseIndex {
            sapling_key,
            height: skip_up_to_height,
        };

        self.zs_insert(&storage.sapling_tx_ids_cf(), index, Vec::new());
    }
}
