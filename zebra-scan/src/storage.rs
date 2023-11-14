//! Store viewing keys and results of the scan.

use std::collections::HashMap;

use zebra_chain::{block::Height, transaction::Hash};

/// Store key info and results of the scan.
#[allow(dead_code)]
pub struct Storage {
    /// The key and an optional birthday for it.
    keys: HashMap<String, Option<Height>>,

    /// The key and the related transaction id.
    results: HashMap<String, Vec<Hash>>,
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
    pub fn add_key(&mut self, key: String, birthday: Option<Height>) {
        self.keys.insert(key, birthday);
    }

    /// Add a result to the storage.
    pub fn add_result(&mut self, key: String, txid: Hash) {
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
    pub fn get_keys(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }
}
