//! [`EvictionList`] represents the transaction eviction list with
//! efficient operations.
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use zebra_chain::transaction;

/// An eviction list that allows Zebra to efficiently add entries, get entries,
/// and remove older entries in the order they were inserted.
pub struct EvictionList {
    // Maps each TXID in the list to the most recent instant they were added.
    unique_entries: HashMap<transaction::Hash, Instant>,
    // The same as `unique_entries` but in the order they were inserted.
    ordered_entries: VecDeque<transaction::Hash>,
    // The maximum size of `unique_entries`.
    max_size: usize,
    /// The mempool transaction eviction age limit.
    /// Same as [`Config::eviction_memory_time`][1].
    ///
    /// [1]: super::super::Config::eviction_memory_time
    eviction_memory_time: Duration,
}

impl EvictionList {
    /// Create a new [`EvictionList`] with the given maximum size and
    /// eviction time.
    pub fn new(max_size: usize, eviction_memory_time: Duration) -> Self {
        Self {
            unique_entries: Default::default(),
            ordered_entries: Default::default(),
            max_size,
            eviction_memory_time,
        }
    }

    /// Inserts a TXID in the list, keeping track of the time it was inserted.
    ///
    /// All entries older than [`EvictionList::eviction_memory_time`] will be removed.
    ///
    /// # Panics
    ///
    /// If the TXID is already in the list.
    ///
    pub fn insert(&mut self, key: transaction::Hash) {
        // From https://zips.z.cash/zip-0401#specification:
        // > Nodes SHOULD remove transactions from RecentlyEvicted that were evicted more than
        // > mempoolevictionmemoryminutes minutes ago. This MAY be done periodically,
        // > and/or just before RecentlyEvicted is accessed when receiving a transaction.
        self.prune_old();
        // > Add the txid and the current time to RecentlyEvicted, dropping the oldest entry
        // > in RecentlyEvicted if necessary to keep it to at most eviction_memory_entries entries.
        if self.len() >= self.max_size {
            self.pop_front();
        }
        let value = Instant::now();
        let old_value = self.unique_entries.insert(key, value);
        // It should be impossible for an already-evicted transaction to be evicted
        // again since transactions are not added to the mempool if they are evicted,
        // and the mempool doesn't allow inserting two transactions with the same
        // hash (they would conflict).
        assert_eq!(
            old_value, None,
            "an already-evicted transaction should not be evicted again"
        );
        self.ordered_entries.push_back(key)
    }

    /// Checks if the given TXID is in the list.
    pub fn contains_key(&self, txid: &transaction::Hash) -> bool {
        if let Some(evicted_at) = self.unique_entries.get(txid) {
            // Since the list is pruned only in mutable functions, make sure
            // we take expired items into account.
            if !self.has_expired(evicted_at) {
                return true;
            }
        }
        false
    }

    /// Get the size of the list.
    //
    // Note: if this method being mutable becomes an issue, it's possible
    // to compute the number of expired transactions and subtract,
    // at the cost of `O(len + expired)` performance each time the method is called.
    //
    // Currently the performance is `O(expired)` for the first call, then `O(1)` until the next expiry.
    pub fn len(&mut self) -> usize {
        self.prune_old();
        self.unique_entries.len()
    }

    /// Clear the list.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.unique_entries.clear();
        self.ordered_entries.clear();
    }

    /// Prune TXIDs that are older than `eviction_time` ago.
    ///
    // This method is public because ZIP-401 states about pruning:
    // > This MAY be done periodically,
    pub fn prune_old(&mut self) {
        while let Some(txid) = self.front() {
            let evicted_at = self
                .unique_entries
                .get(txid)
                .unwrap_or_else(|| panic!("all entries should exist in both ordered_entries and unique_entries, missing {:?} in unique_entries", txid));
            if self.has_expired(evicted_at) {
                self.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get the oldest TXID in the list.
    fn front(&self) -> Option<&transaction::Hash> {
        self.ordered_entries.front()
    }

    /// Removes the first element and returns it, or `None` if the `EvictionList`
    /// is empty.
    fn pop_front(&mut self) -> Option<transaction::Hash> {
        if let Some(key) = self.ordered_entries.pop_front() {
            let removed = self.unique_entries.remove(&key);
            assert!(
                removed.is_some(),
                "all entries should exist in both ordered_entries and unique_entries, missing {:?} in unique_entries",
                key
            );
            Some(key)
        } else {
            None
        }
    }

    /// Returns if `evicted_at` is considered expired considering the current
    /// time and the configured eviction time.
    fn has_expired(&self, evicted_at: &Instant) -> bool {
        let now = Instant::now();
        (now - *evicted_at) > self.eviction_memory_time
    }
}
