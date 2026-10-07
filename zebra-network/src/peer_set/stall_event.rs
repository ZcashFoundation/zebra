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

/// The existing empty/non-empty classification of a find response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StallOutcome {
    Stall,
    Clear,
}
