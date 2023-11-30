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
    pub fn sapling_result_for_key_and_block(
        &self,
        index: &SaplingScannedDatabaseIndex,
    ) -> Vec<SaplingScannedResult> {
        self.db
            .zs_get(&self.sapling_tx_ids_cf(), &index)
            .unwrap_or_default()
    }

    // Column family convenience methods

    /// Returns a handle to the `sapling_tx_ids` column family.
    pub(crate) fn sapling_tx_ids_cf(&self) -> impl AsColumnFamilyRef + '_ {
        self.db.cf_handle(SAPLING_TX_IDS).unwrap()
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
        let birthday_height = birthday_height
            .unwrap_or(min_birthday_height)
            .max(min_birthday_height);

        let index = SaplingScannedDatabaseIndex {
            sapling_key,
            height: birthday_height,
        };

        self.zs_insert(&storage.sapling_tx_ids_cf(), index, Vec::new());
    }
}
