//! Mock [`PeerObserver`] for use in tests.

use crate::{meta_addr::MetaAddr, PeerObserver};

/// A mock [`PeerObserver`] implementation that's always empty.
#[derive(Default, Clone)]
pub struct MockPeerObserver {
    /// Return value for mock `recently_live_peers` method.
    recently_live_peers: Vec<MetaAddr>,
}

impl MockPeerObserver {
    /// Creates a new [`MockPeerObserver`]
    pub fn new(recently_live_peers: Vec<MetaAddr>) -> Self {
        Self {
            recently_live_peers,
        }
    }
}

impl PeerObserver for MockPeerObserver {
    fn recently_live_peers(&self, _now: chrono::DateTime<chrono::Utc>) -> Vec<MetaAddr> {
        self.recently_live_peers.clone()
    }
}
