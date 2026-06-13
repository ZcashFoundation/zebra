//! Recent request counts with rotation-based decay.

use std::{collections::HashMap, hash::Hash, mem};

/// Tracks each caller's recent request count.
///
/// Counts decay by rotation, like Zebra's inventory registry: [`record`]
/// increments a key's count in the `current` generation, and [`rotate`] moves
/// the `current` generation to `prev` and drops the old `prev` generation.
/// A key's count is the sum of its `current` and `prev` entries, so it covers
/// at least one and at most two rotation intervals.
///
/// This bounds the per-caller state: every key is dropped at most two
/// rotation intervals after its last request, without tracking per-request
/// timestamps.
///
/// [`record`]: Self::record
/// [`rotate`]: Self::rotate
#[derive(Debug)]
pub(crate) struct RecentRequestCounts<K> {
    /// Request counts recorded since the last rotation.
    current: HashMap<K, u64>,

    /// Request counts recorded in the rotation interval before last.
    prev: HashMap<K, u64>,
}

impl<K> RecentRequestCounts<K> {
    /// Returns an empty set of counts.
    pub(crate) fn new() -> Self {
        Self {
            current: HashMap::new(),
            prev: HashMap::new(),
        }
    }

    /// Rotates the count generations, expiring counts recorded before the
    /// last rotation.
    pub(crate) fn rotate(&mut self) {
        self.prev = mem::take(&mut self.current);
    }
}

impl<K> RecentRequestCounts<K>
where
    K: Eq + Hash,
{
    /// Records a request from `key`, and returns its updated recent request
    /// count.
    ///
    /// The returned count includes the request being recorded, so it is
    /// always at least 1: external requests always sort after internal
    /// requests, which have priority 0.
    pub(crate) fn record(&mut self, key: K) -> u64 {
        let prev = self.prev.get(&key).copied().unwrap_or_default();
        let current = self.current.entry(key).or_default();
        // The count is bounded by the caller's request rate over two rotation
        // intervals, but saturate anyway: a saturated count just keeps the
        // caller at the lowest priority.
        *current = current.saturating_add(1);

        current.saturating_add(prev)
    }
}
