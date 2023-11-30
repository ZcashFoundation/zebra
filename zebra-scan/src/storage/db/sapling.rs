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

use zebra_state::{
    AsColumnFamilyRef, ReadDisk, SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex,
    SaplingScannedResult, WriteDisk,
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
}
