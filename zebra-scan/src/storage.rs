//! Store viewing keys and results of the scan.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{block::Height, parameters::Network};
use zebra_state::TransactionIndex;

use crate::config::Config;

pub mod db;

// Public types and APIs
pub use db::{SaplingScannedResult, SaplingScanningKey};

/// We insert an empty results entry to the database every this interval for each stored key,
/// so we can track progress.
pub const INSERT_CONTROL_INTERVAL: u32 = 1_000;

/// Store key info and results of the scan.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so clones of the scanner storage represent the same database instance.
/// When the final clone is dropped, the database is closed.
#[derive(Clone, Debug)]
pub struct Storage {
    // Configuration
    //
    // This configuration cannot be modified after the database is initialized,
    // because some clones would have different values.
    //
    // TODO: add config if needed?

    // Owned State
    //
    // Everything contained in this state must be shared by all clones, or read-only.
    //
    /// The underlying database.
    ///
    /// `rocksdb` allows reads and writes via a shared reference,
    /// so this database object can be freely cloned.
    /// The last instance that is dropped will close the underlying database.
    db: db::ScannerDb,
}

impl Storage {
    /// Opens and returns the on-disk scanner results storage for `config` and `network`.
    /// If there is no existing storage, creates a new storage on disk.
    ///
    /// Birthdays and scanner progress are marked by inserting an empty result for that height.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while creating or reading database files, so it must be inside
    /// spawn_blocking() in async code.
    pub fn new(config: &Config, network: &Network, read_only: bool) -> Self {
        let mut storage = Self::new_db(config, network, read_only);

        for (sapling_key, birthday) in config.sapling_keys_to_scan.iter() {
            storage.add_sapling_key(sapling_key, Some(zebra_chain::block::Height(*birthday)));
        }

        storage
    }

    /// Add a sapling key to the storage.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while writing database files, so it must be inside spawn_blocking()
    /// in async code.
    pub fn add_sapling_key(
        &mut self,
        sapling_key: &SaplingScanningKey,
        birthday: impl Into<Option<Height>>,
    ) {
        let birthday = birthday.into();

        // It's ok to write some keys and not others during shutdown, so each key can get its own
        // batch. (They will be re-written on startup anyway.)
        self.insert_sapling_key(sapling_key, birthday);
    }

    /// Returns all the keys and their last scanned heights.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while reading database files, so it must be inside spawn_blocking()
    /// in async code.
    pub fn sapling_keys_last_heights(&self) -> HashMap<SaplingScanningKey, Height> {
        self.sapling_keys_and_last_scanned_heights()
    }

    /// Add the sapling results for `height` to the storage. The results can be any map of
    /// [`TransactionIndex`] to [`SaplingScannedResult`].
    ///
    /// All the results for the same height must be written at the same time, to avoid partial
    /// writes during shutdown.
    ///
    /// Also adds empty progress tracking entries every `INSERT_CONTROL_INTERVAL` blocks if needed.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while writing database files, so it must be inside spawn_blocking()
    /// in async code.
    pub fn add_sapling_results(
        &mut self,
        sapling_key: &SaplingScanningKey,
        height: Height,
        sapling_results: BTreeMap<TransactionIndex, SaplingScannedResult>,
    ) {
        self.insert_sapling_results(sapling_key, height, sapling_results)
    }

    /// Returns all the results for a sapling key, for every scanned block height.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while reading database files, so it must be inside spawn_blocking()
    /// in async code.
    pub fn sapling_results(
        &self,
        sapling_key: &SaplingScanningKey,
    ) -> BTreeMap<Height, Vec<SaplingScannedResult>> {
        self.sapling_results_for_key(sapling_key)
    }
}
