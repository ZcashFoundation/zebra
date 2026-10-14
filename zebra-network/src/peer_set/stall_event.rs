//! Events from peer services that update find-response stall tracking.

use crate::PeerSocketAddr;

/// A response classification or connection closure delivered to the peer set.
#[derive(Debug)]
pub(crate) enum PeerStallEvent {
    Response {
        peer: PeerSocketAddr,
        connection_id: ConnectionId,
        outcome: StallOutcome,
    },
    #[allow(dead_code)]
    ConnectionClosed {
        peer: PeerSocketAddr,
        connection_id: ConnectionId,
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
