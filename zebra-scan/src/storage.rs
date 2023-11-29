//! Store viewing keys and results of the scan.

#![allow(dead_code)]

use std::collections::HashMap;

use zebra_chain::{block::Height, transaction::Hash};

pub mod db;

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
pub type SaplingScanningKey = String;

/// Store key info and results of the scan.
pub struct Storage {
    /// The sapling key and an optional birthday for it.
    sapling_keys: HashMap<SaplingScanningKey, Option<Height>>,

    /// The sapling key and the related transaction id.
    sapling_results: HashMap<SaplingScanningKey, Vec<Hash>>,
}

impl Storage {
    /// Create a new storage.
    pub fn new() -> Self {
        Self {
            sapling_keys: HashMap::new(),
            sapling_results: HashMap::new(),
        }
    }

    /// Add a sapling key to the storage.
    pub fn add_sapling_key(&mut self, key: SaplingScanningKey, birthday: Option<Height>) {
        self.sapling_keys.insert(key, birthday);
    }

    /// Add a sapling result to the storage.
    pub fn add_sapling_result(&mut self, key: SaplingScanningKey, txid: Hash) {
        if let Some(results) = self.sapling_results.get_mut(&key) {
            results.push(txid);
        } else {
            self.sapling_results.insert(key, vec![txid]);
        }
    }

    /// Get the results of a sapling key.
    pub fn get_sapling_results(&self, key: &str) -> Vec<Hash> {
        self.sapling_results.get(key).cloned().unwrap_or_default()
    }

    /// Get all keys and their birthdays.
    pub fn get_sapling_keys(&self) -> HashMap<String, Option<Height>> {
        self.sapling_keys.clone()
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}
