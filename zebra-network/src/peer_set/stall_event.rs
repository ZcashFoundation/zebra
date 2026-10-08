//! Events from peer services that update find-response stall tracking.
//!
//! Response futures retain only sender clones and connection identities, not
//! [`ConnectionGuard`]s. Their lifetime therefore cannot delay closure reporting.
//! These events retain the immediate response policy; deferred consumer feedback
//! is represented separately by the stall tracker's existing feedback API.

use tokio::sync::mpsc;

use crate::PeerSocketAddr;

/// A response classification or connection closure delivered to the peer set.
#[derive(Debug)]
pub(crate) enum PeerStallEvent {
    Response {
        peer: PeerSocketAddr,
        connection_id: ConnectionId,
        outcome: StallOutcome,
    },
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

/// Reports closure when the owning peer service is dropped, independently of responses.
#[derive(Debug)]
pub(crate) struct ConnectionGuard {
    peer: PeerSocketAddr,
    connection_id: ConnectionId,
    sender: mpsc::UnboundedSender<PeerStallEvent>,
}

impl ConnectionGuard {
    /// Creates a guard that reports closure for `peer` and `connection_id` on drop.
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

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let _ = self.sender.send(PeerStallEvent::ConnectionClosed {
            peer: self.peer,
            connection_id: self.connection_id,
        });
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
