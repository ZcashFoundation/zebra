//! Tracks peers that consistently return stalled `FindBlocks` or `FindHeaders`
//! responses, so the peer set can disconnect them.
//!
//! A peer returning a single empty response may just be syncing itself; a peer
//! that does so repeatedly stalls the syncer by forcing retries to others. The
//! counter is per-peer and resets when the response consumer marks a response
//! useful.
//!
//! Only applies to `FindBlocks` and `FindHeaders`. An empty response to
//! `BlocksByHash`/`TransactionsById` is a legitimate "I don't have this
//! inventory" answer, so those don't feed the tracker.

use std::{
    collections::{HashMap, VecDeque},
    fmt::{self, Debug, Formatter},
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use crate::PeerSocketAddr;

#[cfg(any(test, feature = "proptest-impl"))]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Consecutive stalled `FindBlocks`/`FindHeaders` responses tolerated before
/// the peer set disconnects a peer.
pub(super) const FIND_RESPONSE_STALL_THRESHOLD: usize = 3;

#[derive(Default)]
pub(super) struct FindResponseStallTracker {
    counts: HashMap<PeerSocketAddr, usize>,
    pending: HashMap<PeerSocketAddr, VecDeque<PendingFindResponse>>,
}

/// A routed find request waiting for an ordered [`FindResponseOutcome`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PendingFindResponse {
    request_id: FindRequestId,
    outcome: Option<FindResponseOutcome>,
}

impl FindResponseStallTracker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Registers `peer` and `request_id` before the response future is polled.
    pub(super) fn begin_request(&mut self, peer: PeerSocketAddr, request_id: FindRequestId) {
        self.pending
            .entry(peer)
            .or_default()
            .push_back(PendingFindResponse {
                request_id,
                outcome: None,
            });
    }

    /// Records a [`FindResponseEvent`] and applies outcomes in request order.
    ///
    /// Returns `true` if the peer reaches the stall threshold.
    pub(super) fn record_response(&mut self, event: FindResponseEvent) -> bool {
        let Some(responses) = self.pending.get_mut(&event.peer) else {
            return false;
        };
        let Some(response) = responses
            .iter_mut()
            .find(|response| response.request_id == event.request_id)
        else {
            return false;
        };

        if response.outcome.is_some() {
            return false;
        }
        response.outcome = Some(event.outcome);

        self.apply_completed_outcomes(event.peer)
    }

    /// Applies completed response outcomes for `peer` in request order.
    fn apply_completed_outcomes(&mut self, peer: PeerSocketAddr) -> bool {
        let Some(mut responses) = self.pending.remove(&peer) else {
            return false;
        };

        while let Some(outcome) = responses.front().and_then(|response| response.outcome) {
            responses.pop_front();
            match outcome {
                FindResponseOutcome::Useful => self.counts.remove(&peer),
                FindResponseOutcome::Stalled if self.record_stall(peer) => {
                    self.clear(peer);
                    return true;
                }
                FindResponseOutcome::Stalled | FindResponseOutcome::Unclassified => None,
            };
        }

        if !responses.is_empty() {
            self.pending.insert(peer, responses);
        }

        false
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
        self.pending.remove(&addr);
    }
}

/// An opaque capability for classifying one peer's `FindBlocks` response.
///
/// Cloned handles share a one-shot outcome. Dropping the final unclassified
/// handle reports no judgment about the peer.
#[derive(Clone)]
pub struct FindResponseFeedback {
    inner: Arc<FindResponseFeedbackInner>,
}

/// Shared one-shot reporting state for cloned [`FindResponseFeedback`] handles.
struct FindResponseFeedbackInner {
    peer: PeerSocketAddr,
    request_id: FindRequestId,
    sender: Mutex<Option<mpsc::UnboundedSender<FindResponseEvent>>>,
}

impl FindResponseFeedback {
    /// Creates a [`FindResponseFeedback`] attributed to `peer` and `request_id`.
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

    /// Marks this response as useful to the consumer.
    pub fn mark_useful(self) {
        self.inner.report(FindResponseOutcome::Useful);
    }

    /// Marks this response as stalled because it was unusable to the consumer.
    pub fn mark_stalled(self) {
        self.inner.report(FindResponseOutcome::Stalled);
    }
}

impl FindResponseFeedbackInner {
    /// Reports `outcome` if no other handle has classified this response.
    fn report(&self, outcome: FindResponseOutcome) {
        let sender = self
            .sender
            .lock()
            .expect("the feedback sender mutex is not held across panicking operations")
            .take();

        if let Some(sender) = sender {
            let _ = sender.send(FindResponseEvent::new(self.peer, self.request_id, outcome));
        }
    }
}

impl Debug for FindResponseFeedback {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("FindResponseFeedback")
            .field("request_id", &self.inner.request_id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for FindResponseFeedback {
    fn eq(&self, other: &Self) -> bool {
        self.inner.peer == other.inner.peer && self.inner.request_id == other.inner.request_id
    }
}

// Deriving `Eq` would require the ignored feedback channel state to implement `Eq`.
impl Eq for FindResponseFeedback {}

impl Drop for FindResponseFeedbackInner {
    fn drop(&mut self) {
        self.report(FindResponseOutcome::Unclassified);
    }
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

/// Observes classifications emitted by a [`FindResponseFeedback`] in tests.
#[cfg(any(test, feature = "proptest-impl"))]
pub struct FindResponseFeedbackObserver {
    receiver: Mutex<mpsc::UnboundedReceiver<FindResponseEvent>>,
}

#[cfg(any(test, feature = "proptest-impl"))]
impl FindResponseFeedback {
    /// Creates a [`FindResponseFeedback`] and observer for tests.
    pub fn new_for_test() -> (Self, FindResponseFeedbackObserver) {
        let peer = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8233)).into();
        let (sender, receiver) = mpsc::unbounded_channel();

        (
            Self::new(peer, FindRequestId::from(0), sender),
            FindResponseFeedbackObserver {
                receiver: Mutex::new(receiver),
            },
        )
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl FindResponseFeedbackObserver {
    /// Returns `Some(true)` for [`FindResponseOutcome::Useful`], `Some(false)`
    /// for [`FindResponseOutcome::Stalled`], and `None` while unclassified.
    pub fn outcome(&self) -> Option<bool> {
        let mut receiver = self.receiver.lock().unwrap_or_else(|_| {
            panic!("the test receiver mutex is not held across panicking operations")
        });

        match receiver.try_recv() {
            Ok(FindResponseEvent {
                outcome: FindResponseOutcome::Useful,
                ..
            }) => Some(true),
            Ok(FindResponseEvent {
                outcome: FindResponseOutcome::Stalled,
                ..
            }) => Some(false),
            Ok(FindResponseEvent {
                outcome: FindResponseOutcome::Unclassified,
                ..
            }) => None,
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => None,
        }
    }
}

#[cfg(test)]
mod tests;
