//! Recent request costs with rotation-based decay.

use std::{collections::HashMap, hash::Hash, mem, time::Duration};

/// The base cost of one request, in cost points.
///
/// Every request costs at least one point, so callers with any recent
/// activity always sort after internal requests, which have priority 0.
pub(crate) const BASE_REQUEST_COST: u64 = 1;

/// The inner-service response time equal to one cost point.
///
/// One cheap request costs the same as this much response time, so a
/// caller's priority depends on both how *often* it asks and how *expensive*
/// its requests are to serve: a few slow requests can outweigh many fast
/// ones.
pub(crate) const RESPONSE_TIME_PER_COST_POINT: Duration = Duration::from_millis(10);

/// Tracks each caller's recent request cost.
///
/// A caller's cost is [`BASE_REQUEST_COST`] per request, plus one point per
/// [`RESPONSE_TIME_PER_COST_POINT`] of inner-service response time, recorded
/// when each response completes.
///
/// Costs decay by rotation, like Zebra's inventory registry:
/// [`record_request`] and [`record_response_time`] add to a key's cost in
/// the `current` generation, and [`rotate`] moves the `current` generation
/// to `prev` and drops the old `prev` generation. A key's cost is the sum of
/// its `current` and `prev` entries, so it covers at least one and at most
/// two rotation intervals.
///
/// This bounds the per-caller state: every key is dropped at most two
/// rotation intervals after its last activity, without tracking per-request
/// timestamps.
///
/// [`record_request`]: Self::record_request
/// [`record_response_time`]: Self::record_response_time
/// [`rotate`]: Self::rotate
#[derive(Debug)]
pub(crate) struct RecentRequestCosts<K> {
    /// Request costs recorded since the last rotation.
    current: HashMap<K, u64>,

    /// Request costs recorded in the rotation interval before last.
    prev: HashMap<K, u64>,
}

impl<K> RecentRequestCosts<K> {
    /// Returns an empty set of costs.
    pub(crate) fn new() -> Self {
        Self {
            current: HashMap::new(),
            prev: HashMap::new(),
        }
    }

    /// Rotates the cost generations, expiring costs recorded before the
    /// last rotation.
    pub(crate) fn rotate(&mut self) {
        self.prev = mem::take(&mut self.current);
    }
}

impl<K> RecentRequestCosts<K>
where
    K: Eq + Hash,
{
    /// Records the base cost of a request from `key`, and returns its
    /// updated recent request cost.
    ///
    /// The returned cost includes the request being recorded, so it is
    /// always at least [`BASE_REQUEST_COST`]: external requests always sort
    /// after internal requests, which have priority 0.
    pub(crate) fn record_request(&mut self, key: K) -> u64 {
        self.add(key, BASE_REQUEST_COST)
    }

    /// Records the inner service taking `response_time` to respond to a
    /// request from `key`.
    pub(crate) fn record_response_time(&mut self, key: K, response_time: Duration) {
        // Truncating an unrealistically long response time to u64
        // milliseconds cannot undercount it: u64::MAX milliseconds is over
        // 500 million years.
        let response_millis = response_time.as_millis().min(u128::from(u64::MAX)) as u64;
        let quantum = u64::try_from(RESPONSE_TIME_PER_COST_POINT.as_millis())
            .expect("the cost point quantum is far below u64::MAX milliseconds");
        let points = response_millis / quantum;

        if points > 0 {
            self.add(key, points);
        }
    }

    /// Adds `points` to `key`\'s cost in the current generation, and returns
    /// its total recent cost.
    fn add(&mut self, key: K, points: u64) -> u64 {
        let prev = self.prev.get(&key).copied().unwrap_or_default();
        let current = self.current.entry(key).or_default();
        // The cost is bounded by the caller\'s activity over two rotation
        // intervals, but saturate anyway: a saturated cost just keeps the
        // caller at the lowest priority.
        *current = current.saturating_add(points);

        current.saturating_add(prev)
    }
}
