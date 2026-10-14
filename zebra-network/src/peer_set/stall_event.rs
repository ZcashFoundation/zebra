//! Events from peer services that update find-response stall tracking.

use crate::PeerSocketAddr;

/// An immediate response classification delivered to the peer set.
#[derive(Debug)]
pub(crate) enum PeerStallEvent {
    Response {
        peer: PeerSocketAddr,
        outcome: StallOutcome,
    },
}

/// Identifies one admitted connection within a peer set, independently of its address.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionId(u64);

impl From<u64> for ConnectionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

/// The existing empty/non-empty classification of a find response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StallOutcome {
    Stall,
    Clear,
}
