//! Store viewing keys and results of the scan.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
};
use zebra_state::{
    SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex, TransactionIndex, TransactionLocation,
};

use crate::config::Config;

pub mod db;

// Public types and APIs
pub use db::{SaplingScannedResult, SaplingScanningKey};

use self::db::ScannerWriteBatch;

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
    pub fn new(config: &Config, network: Network) -> Self {
        let mut storage = Self::new_db(config, network);

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
        let mut batch = ScannerWriteBatch::default();

        batch.insert_sapling_key(self, sapling_key, birthday);

        self.write_batch(batch);
    }

    /// Returns all the keys and their birthdays.
    ///
    /// Birthdays are adjusted to sapling activation if they are too low or missing.
    ///
    /// # Performance / Hangs
    ///
    /// This method can block while reading database files, so it must be inside spawn_blocking()
    /// in async code.
    pub fn sapling_keys(&self) -> HashMap<SaplingScanningKey, Height> {
        self.sapling_keys_and_birthday_heights()
    }

    /// Add the sapling results for `height` to the storage. The results can be any map of
    /// [`TransactionIndex`] to [`SaplingScannedResult`].
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
        // We skip heights that have one or more results, so the results for each height must be
        // in a single batch.
        let mut batch = ScannerWriteBatch::default();

        for (index, sapling_result) in sapling_results {
            let index = SaplingScannedDatabaseIndex {
                sapling_key: sapling_key.clone(),
                tx_loc: TransactionLocation::from_parts(height, index),
            };

            let entry = SaplingScannedDatabaseEntry {
                index,
                value: Some(sapling_result),
            };

            batch.insert_sapling_result(self, entry);
        }

        self.write_batch(batch);
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

    // Parameters

    /// Returns the minimum sapling birthday height for the configured network.
    pub fn min_sapling_birthday_height(&self) -> Height {
        // Assume that the genesis block never contains shielded inputs or outputs.
        //
        // # Consensus
        //
        // For Zcash mainnet and the public testnet, Sapling activates above genesis,
        // so this is always true.
        NetworkUpgrade::Sapling
            .activation_height(self.network())
            .unwrap_or(Height(0))
    }
}
