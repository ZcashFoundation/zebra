//! Store viewing keys and results of the scan.

use std::collections::HashMap;

use zebra_chain::{block::Height, transaction::Hash};

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
pub type SaplingScanningKey = String;

/// Store key info and results of the scan.
#[allow(dead_code)]
pub struct Storage {
    /// The key and an optional birthday for it.
    keys: HashMap<SaplingScanningKey, Option<Height>>,

    /// The key and the related transaction id.
    results: HashMap<SaplingScanningKey, Vec<Hash>>,
}

#[allow(dead_code)]
impl Storage {
    /// Create a new storage.
    pub fn new() -> Self {
        Self {
            keys: HashMap::new(),
            results: HashMap::new(),
        }
    }

    /// Add a key to the storage.
    pub fn add_key(&mut self, key: SaplingScanningKey, birthday: Option<Height>) {
        self.keys.insert(key, birthday);
    }

    /// Add a result to the storage.
    pub fn add_result(&mut self, key: SaplingScanningKey, txid: Hash) {
        if let Some(results) = self.results.get_mut(&key) {
            results.push(txid);
        } else {
            self.results.insert(key, vec![txid]);
        }
    }

    /// Get the results of a key.
    pub fn get_results(&self, key: &str) -> Option<&Vec<Hash>> {
        self.results.get(key)
    }

    /// Get all keys.
    pub fn get_keys(&self) -> Vec<SaplingScanningKey> {
        self.keys.keys().cloned().collect()
    }
}
