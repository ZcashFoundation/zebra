//! Tracks peers that consistently return empty or failed `FindBlocks` or
//! `FindHeaders` responses, so the peer set can disconnect them.
//!
//! A peer returning a single empty response may just be syncing itself; a peer
//! that does so repeatedly stalls the syncer by forcing retries to others. The
//! counter is per-peer and resets on any useful (non-empty) response.
//!
//! Only applies to `FindBlocks` and `FindHeaders`. An empty response to
//! `BlocksByHash`/`TransactionsById` is a legitimate "I don't have this
//! inventory" answer, so those don't feed the tracker.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use crate::PeerSocketAddr;

/// Consecutive empty or failed `FindBlocks`/`FindHeaders` responses tolerated
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

    /// Clears tracking for a peer that sent a useful response or disconnected.
    pub(super) fn clear(&mut self, addr: PeerSocketAddr) {
        self.counts.remove(&addr);
    }
}

/// A shared feedback capability whose reporting is not implemented yet.
#[allow(dead_code)]
#[derive(Clone)]
pub struct FindResponseFeedback {
    inner: Arc<FindResponseFeedbackInner>,
}

/// Shared one-shot reporting state for cloned [`FindResponseFeedback`] handles.
#[allow(dead_code)]
struct FindResponseFeedbackInner {
    peer: PeerSocketAddr,
    request_id: FindRequestId,
    sender: Mutex<Option<mpsc::UnboundedSender<FindResponseEvent>>>,
}

#[allow(dead_code)]
impl FindResponseFeedback {
    /// Creates a [`FindResponseFeedback`] attributed to `peer` and `request_id`.
    #[allow(dead_code)]
    pub(super) fn new(
        peer: PeerSocketAddr,
        request_id: FindRequestId,
        sender: mpsc::UnboundedSender<FindResponseEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(FindResponseFeedbackInner {
                peer,
                request_id,
                sender: Mutex::new(Some(sender)),
            }),
        }
    }

    /// Consumes this handle without reporting usefulness yet.
    pub fn mark_useful(self) {}

    /// Consumes this handle without reporting a stall yet.
    pub fn mark_stalled(self) {}
}

/// A peer-set identity that preserves routed find-request order.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub(super) struct FindRequestId(u64);

impl From<u64> for FindRequestId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

/// A response consumer's classification of a routed find request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(super) enum FindResponseOutcome {
    Useful,
    Stalled,
    /// The consumer did not judge the response before abandoning it.
    Unclassified,
}

/// An attributed [`FindResponseOutcome`] sent to the peer set.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct FindResponseEvent {
    pub(super) peer: PeerSocketAddr,
    pub(super) request_id: FindRequestId,
    pub(super) outcome: FindResponseOutcome,
}

#[allow(dead_code)]
impl FindResponseEvent {
    /// Creates a [`FindResponseEvent`] for `peer` and `request_id`.
    pub(super) fn new(
        peer: PeerSocketAddr,
        request_id: FindRequestId,
        outcome: FindResponseOutcome,
    ) -> Self {
        Self {
            peer,
            request_id,
            outcome,
        }
    }
}

#[cfg(test)]
mod tests;
