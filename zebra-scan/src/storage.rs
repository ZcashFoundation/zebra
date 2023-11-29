//! Store viewing keys and results of the scan.

#![allow(dead_code)]

use std::collections::HashMap;

use zebra_chain::{block::Height, parameters::Network};

use crate::config::Config;

pub mod db;

// Public types and APIs
pub use db::{SaplingScannedResult, SaplingScanningKey};

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
    //
    // This database is created but not actually used for results.
    // TODO: replace the fields below with a database instance.
    db: db::ScannerDb,

    /// The sapling key and an optional birthday for it.
    sapling_keys: HashMap<SaplingScanningKey, Option<Height>>,

    /// The sapling key and the related transaction id.
    sapling_results: HashMap<SaplingScanningKey, Vec<SaplingScannedResult>>,
}

impl Storage {
    /// Opens and returns the on-disk scanner results storage for `config` and `network`.
    /// If there is no existing storage, creates a new storage on disk.
    ///
    /// TODO:
    /// New keys in `config` are inserted into the database with their birthday heights. Shielded
    /// activation is the minimum birthday height.
    ///
    /// Birthdays and scanner progress are marked by inserting an empty result for that height.
    pub fn new(config: &Config, network: Network) -> Self {
        let mut storage = Self::new_db(config, network);

        for (key, birthday) in config.sapling_keys_to_scan.iter() {
            storage.add_sapling_key(key.clone(), Some(zebra_chain::block::Height(*birthday)));
        }

        storage
    }

    /// Add a sapling key to the storage.
    pub fn add_sapling_key(&mut self, key: SaplingScanningKey, birthday: Option<Height>) {
        self.sapling_keys.insert(key, birthday);
    }

    /// Add a sapling result to the storage.
    pub fn add_sapling_result(&mut self, key: SaplingScanningKey, txid: SaplingScannedResult) {
        if let Some(results) = self.sapling_results.get_mut(&key) {
            results.push(txid);
        } else {
            self.sapling_results.insert(key, vec![txid]);
        }
    }

    /// Returns all the results for a sapling key, for every scanned block height.
    //
    // TODO: Rust style - remove "get_" from these names
    pub fn get_sapling_results(&self, key: &str) -> Vec<SaplingScannedResult> {
        self.sapling_results.get(key).cloned().unwrap_or_default()
    }

    /// Returns all the keys and their birthdays.
    //
    // TODO: any value below sapling activation as the birthday height, or `None`, should default
    // to sapling activation. This requires the configured network.
    // Return Height not Option<Height>.
    pub fn get_sapling_keys(&self) -> HashMap<SaplingScanningKey, Option<Height>> {
        self.sapling_keys.clone()
    }
}
