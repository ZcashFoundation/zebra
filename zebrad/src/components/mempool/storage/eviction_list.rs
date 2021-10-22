//! [`EvictionList`] represents the transaction eviction list with
//! efficient operations.
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use zebra_chain::transaction;

/// An eviction list that allows to efficiently add entries, get entries,
/// and remove older entries in the order they were inserted.
pub struct EvictionList {
    // Maps each TXID in the list to the most recent instant they were added.
    unique_entries: HashMap<transaction::Hash, Instant>,
    // The entries in the order they were inserted.
    // This can be larger than `unique_entries` if a same txid is added
    // multiple times. Its instant will be overwritten in
    // `unique_entries` but all entries will kept in `unique_entries`.
    ordered_entries: VecDeque<(transaction::Hash, Instant)>,
    // The maximum size of `unique_entries`.
    max_size: usize,
    /// The mempool transaction eviction age limit.
    /// Same as [`Config::eviction_memory_time`].
    eviction_time: Duration,
}

impl EvictionList {
    /// Create a new [`EvictionList`] with the given maximum size and
    /// eviction time.
    pub fn new(max_size: usize, eviction_time: Duration) -> Self {
        Self {
            unique_entries: Default::default(),
            ordered_entries: Default::default(),
            size,
            eviction_time,
        }
    }

    /// Inserts a TXID in the list, keeping track of the time it was inserted.
    ///
    /// If the TXID is already in the list, its insertion time will be updated.
    ///
    /// All entries older than [`EvictionList::eviction_time`] will be removed.
    pub fn insert(&mut self, key: transaction::Hash) {
        self.prune_old();
        while self.len() >= self.size {
            self.pop_front();
        }
        let value = Instant::now();
        let old_value = self.unique_entries.insert(key, value);
        if old_value != Some(value) {
            self.ordered_entries.push_back((key, value))
        }
    }

    /// Checks if the given TXID is in the list.
    pub fn contains_key(&self, txid: &transaction::Hash) -> bool {
        self.unique_entries.contains_key(txid)
    }

    /// Get the size of the list.
    pub fn len(&self) -> usize {
        self.unique_entries.len()
    }

    /// Clear the list.
    pub fn clear(&mut self) {
        self.unique_entries.clear();
        self.ordered_entries.clear();
    }

    /// Prune TXIDs that are older than `eviction_time` ago.
    pub fn prune_old(&mut self) {
        let now = Instant::now();
        while let Some((_txid, evicted_at)) = self.front() {
            if (now - *evicted_at) > self.eviction_time {
                self.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get the oldest TXID in the list.
    ///
    /// If the TXID was refreshed, then the entry can correspond to the older
    /// value.
    fn front(&self) -> Option<&(transaction::Hash, Instant)> {
        self.ordered_entries.front()
    }

    /// Removes the first element and returns it, or `None` if the `EvictionList`
    /// is empty.
    ///
    /// If the TXID was refreshed, then the entry can correspond to the older
    /// value. In that case, the most recent TXID won't be removed from the list
    /// yet.
    fn pop_front(&mut self) -> Option<(transaction::Hash, Instant)> {
        let entry = self.ordered_entries.pop_front();
        if let Some((key, value)) = &entry {
            if self.unique_entries.get(key) == Some(value) {
                self.unique_entries.remove(key);
            }
        }
        entry
    }
}
