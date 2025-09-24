use std::time::Instant;

/// Lightweight connectivity telemetry published by the peer set.
///
/// `ready_peers` counts peers that are currently ready to service requests.
/// `total_peers` counts all peers known to the peer set (ready + busy).
/// `last_updated` records when the peer set last refreshed the counts.
#[derive(Clone, Debug)]
pub struct PeerSetStatus {
    /// Number of peers currently ready to service requests.
    pub ready_peers: usize,
    /// Total number of peers tracked by the peer set (ready + busy).
    pub total_peers: usize,
    /// When this snapshot was generated.
    pub last_updated: Instant,
}

impl PeerSetStatus {
    /// Construct a new status snapshot for the provided counts.
    pub fn new(ready_peers: usize, total_peers: usize) -> Self {
        Self {
            ready_peers,
            total_peers,
            last_updated: Instant::now(),
        }
    }
}

impl Default for PeerSetStatus {
    fn default() -> Self {
        Self::new(0, 0)
    }
}
