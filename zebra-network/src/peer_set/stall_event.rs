//! Events from peer services that update find-response stall tracking.

use tokio::sync::mpsc;

use crate::PeerSocketAddr;

/// An immediate response classification delivered to the peer set.
#[derive(Debug)]
pub(crate) enum PeerStallEvent {
    Response {
        peer: PeerSocketAddr,
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

/// Retains a connection's cleanup identity; drop notification is not implemented yet.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ConnectionGuard {
    peer: PeerSocketAddr,
    connection_id: ConnectionId,
    sender: mpsc::UnboundedSender<PeerStallEvent>,
}

#[allow(dead_code)]
impl ConnectionGuard {
    /// Creates a guard without reporting closure until drop notification is implemented.
    pub(crate) fn new(
        peer: PeerSocketAddr,
        connection_id: ConnectionId,
        sender: mpsc::UnboundedSender<PeerStallEvent>,
    ) -> Self {
        Self {
            peer,
            connection_id,
            sender,
        }
    }
}

/// The existing empty/non-empty classification of a find response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StallOutcome {
    Stall,
    Clear,
}

#[cfg(test)]
mod tests;
