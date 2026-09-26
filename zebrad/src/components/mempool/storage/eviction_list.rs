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
    /// If the TXID is already present (not expected via the normal mempool path;
    /// see the note in the body), its timestamp is refreshed in place rather than
    /// adding a duplicate entry.
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
        // `prune_old` above removes any expired entry for this key, and while a
        // non-expired entry exists `contains_key` keeps the transaction rejected,
        // so it cannot re-enter the mempool to be re-evicted. A key that is still
        // present here is therefore not expected through the normal mempool path.
        // Handle it gracefully rather than panicking, to stay robust to future
        // callers or refactors: the timestamp is refreshed in place by the
        // `insert` below, and a new `ordered_entries` slot is only appended for a
        // genuinely new key. This keeps the two collections consistent in
        // membership. It does not move a refreshed key to the back, so
        // `ordered_entries` may briefly not be in timestamp order; the only
        // effect is that entries behind a refreshed key are pruned no earlier
        // than the refreshed key itself, which is harmless on this unexpected
        // path. See #10690.
        let old_value = self.unique_entries.insert(key, value);
        if old_value.is_none() {
            self.ordered_entries.push_back(key);
        }
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
                .unwrap_or_else(|| panic!("all entries should exist in both ordered_entries and unique_entries, missing {txid:?} in unique_entries"));
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
                "all entries should exist in both ordered_entries and unique_entries, missing {key:?} in unique_entries"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_hash(byte: u8) -> transaction::Hash {
        transaction::Hash([byte; 32])
    }

    /// Regression test for #10690: re-inserting a key whose entry is still live
    /// must not panic, and must keep `unique_entries` and `ordered_entries`
    /// consistent (no phantom ordered entry that would later panic `pop_front`).
    #[test]
    fn reinsert_live_key_is_graceful() {
        let mut list = EvictionList::new(10, Duration::from_secs(3600));
        let key = dummy_hash(1);

        list.insert(key);
        // Re-insert the same key while its entry is still live. Before the fix
        // this tripped an `assert_eq!`.
        list.insert(key);

        assert!(list.contains_key(&key));
        // Exactly one logical entry, with both backing collections in agreement.
        assert_eq!(list.unique_entries.len(), 1);
        assert_eq!(list.ordered_entries.len(), 1);
        assert_eq!(list.len(), 1);
    }

    /// A re-insert of a live key followed by evictions must not desync the two
    /// backing collections: `pop_front` asserts every ordered entry still exists
    /// in `unique_entries`, so a phantom duplicate would panic here.
    #[test]
    fn reinsert_then_fill_past_max_size_does_not_panic() {
        let max_size = 4;
        let mut list = EvictionList::new(max_size, Duration::from_secs(3600));
        let key = dummy_hash(1);

        list.insert(key);
        list.insert(key);

        // Insert enough distinct keys to evict past `max_size`, exercising
        // `pop_front` over the earlier re-inserted key.
        for byte in 2..=(max_size as u8 + 3) {
            list.insert(dummy_hash(byte));
        }

        assert_eq!(list.len(), max_size);
        assert_eq!(list.unique_entries.len(), list.ordered_entries.len());
    }
}
