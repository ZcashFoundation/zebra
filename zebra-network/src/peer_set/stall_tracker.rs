//! Tracks peers that consistently return failed `FindBlocks` or `FindHeaders`
//! responses, so the peer set can disconnect them.
//!
//! Successful responses, including empty ones, reset the per-peer counter.
//! Empty replies can be legitimate when a peer has no blocks beyond our locator;
//! fair request routing prevents them from monopolizing discovery.
//!
//! Only applies to `FindBlocks` and `FindHeaders`. An empty response to
//! `BlocksByHash`/`TransactionsById` is a legitimate "I don't have this
//! inventory" answer, so those don't feed the tracker.

use std::collections::HashMap;

use crate::PeerSocketAddr;

/// Consecutive failed `FindBlocks`/`FindHeaders` responses tolerated
/// before the peer set disconnects a peer.
pub(super) const FIND_RESPONSE_STALL_THRESHOLD: usize = 3;

#[derive(Default)]
pub(super) struct FindResponseStallTracker {
    counts: HashMap<PeerSocketAddr, usize>,
}

impl FindResponseStallTracker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Records a stall for `addr`. Returns `true` once the peer reaches
    /// [`FIND_RESPONSE_STALL_THRESHOLD`] — the caller must then disconnect it.
    /// On threshold the entry is removed, so a reconnected peer starts fresh.
    pub(super) fn record_stall(&mut self, addr: PeerSocketAddr) -> bool {
        let count = self.counts.entry(addr).or_default();
        *count += 1;

        if *count >= FIND_RESPONSE_STALL_THRESHOLD {
            self.counts.remove(&addr);
            true
        } else {
            false
        }
    }

    /// Clears tracking for a peer that sent a successful response or disconnected.
    pub(super) fn clear(&mut self, addr: PeerSocketAddr) {
        self.counts.remove(&addr);
    }
}

#[cfg(test)]
mod tests;
