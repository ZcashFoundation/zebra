//! Per-height peer exclusion sets for the known-hash IBD engine.
//!
//! Kept deliberately minimal (design doc §4.1, C13 resolution): no scoring
//! and no denylist. The engine records which peers explicitly reported a
//! height as `notfound` so its retry policy can tell "this peer lacks the
//! block" apart from transport noise; the sets are local, per-height, and
//! time-bounded — they are cleared on the peer set's inventory-rotation
//! cadence so recovered peers are retried, and dropped as the frontier
//! advances past their heights.
//!
//! The engine never writes to the peer set's inventory registry; these sets
//! are its only (local) memory of `notfound` responses.

use std::{collections::HashMap, time::Duration};

use tokio::time::Instant;

use zebra_chain::block;
use zebra_network::PeerSocketAddr;

/// How often the per-height exclusion sets are cleared.
///
/// Matches the peer set's inventory-registry rotation interval
/// ([`zebra_network::constants::INVENTORY_ROTATION_INTERVAL`]): peer
/// inventory is assumed stale on the same cadence in both places, so a peer
/// that catches up is retried no later than the registry would retry it.
pub const EXCLUSION_ROTATION_INTERVAL: Duration =
    zebra_network::constants::INVENTORY_ROTATION_INTERVAL;

/// Per-height peer exclusion sets and `notfound` accounting.
///
/// Memory bound (design doc §3.3): the exclusion map holds at most one entry
/// per window slot (`O(span)`), each a short list of socket addresses, and
/// the per-peer counters are `O(connected peers)`; no block data is stored.
#[derive(Debug)]
pub struct PeerStats {
    /// Peers that explicitly reported each height as `notfound`.
    ///
    /// A `Vec` per height: exclusion sets are nearly always 0–2 entries, so
    /// linear scans beat a set.
    excluded: HashMap<block::Height, Vec<PeerSocketAddr>>,

    /// The number of explicit `notfound` responses attributed to each peer,
    /// for the `ibd.peer.notfound.count` metric and stall diagnostics.
    not_found_counts: HashMap<PeerSocketAddr, u64>,

    /// When the exclusion sets were last cleared.
    last_rotation: Instant,
}

impl PeerStats {
    /// Returns empty stats, with the rotation clock starting at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            excluded: HashMap::new(),
            not_found_counts: HashMap::new(),
            last_rotation: now,
        }
    }

    /// Records an explicit `notfound` for `height`, excluding `peer` when the
    /// responding peer is known.
    pub fn record_not_found(&mut self, height: block::Height, peer: Option<PeerSocketAddr>) {
        let Some(peer) = peer else { return };

        self.exclude(height, peer);

        *self.not_found_counts.entry(peer).or_default() += 1;

        metrics::counter!("ibd.peer.notfound.count", "addr" => peer.to_string()).increment(1);
    }

    /// Excludes `peer` for `height`, steering the next fetch of that height
    /// to a different peer.
    ///
    /// Used for `notfound` responses and for peers whose delivered copy
    /// failed verification or its state commit (design doc §4.3, §4.6).
    pub fn exclude(&mut self, height: block::Height, peer: PeerSocketAddr) {
        let excluded = self.excluded.entry(height).or_default();
        if !excluded.contains(&peer) {
            excluded.push(peer);
        }
    }

    /// Returns the peers excluded for `height`.
    pub fn excluded_peers(&self, height: block::Height) -> &[PeerSocketAddr] {
        self.excluded
            .get(&height)
            .map_or(&[], |excluded| excluded.as_slice())
    }

    /// Drops exclusion sets for heights below `height`.
    ///
    /// Called as the commit frontier advances, so the map stays bounded by
    /// the window span.
    pub fn release_below(&mut self, height: block::Height) {
        self.excluded.retain(|h, _| *h >= height);
    }

    /// Clears every exclusion set if [`EXCLUSION_ROTATION_INTERVAL`] has
    /// passed, so peers that have since caught up are retried.
    ///
    /// The per-peer `notfound` counters are diagnostics, not exclusions, and
    /// survive rotation.
    pub fn maybe_rotate(&mut self, now: Instant) {
        if now.duration_since(self.last_rotation) >= EXCLUSION_ROTATION_INTERVAL {
            self.excluded.clear();
            self.last_rotation = now;
        }
    }

    /// The total number of (height, peer) exclusion entries (for tests and
    /// diagnostics).
    pub fn excluded_len(&self) -> usize {
        self.excluded.values().map(Vec::len).sum()
    }

    /// The total number of explicit `notfound` responses recorded against
    /// `peer`.
    pub fn not_found_count(&self, peer: PeerSocketAddr) -> u64 {
        self.not_found_counts
            .get(&peer)
            .copied()
            .unwrap_or_default()
    }
}
