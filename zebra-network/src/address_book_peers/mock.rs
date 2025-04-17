//! Mock [`AddressBookPeers`] for use in tests.

use crate::{meta_addr::MetaAddr, AddressBookPeers};

/// A mock [`AddressBookPeers`] implementation that's always empty.
#[derive(Default, Clone)]
pub struct MockAddressBookPeers {
    /// Return value for mock `recently_live_peers` method.
    recently_live_peers: Vec<MetaAddr>,
}

impl MockAddressBookPeers {
    /// Creates a new [`MockAddressBookPeers`]
    pub fn new(recently_live_peers: Vec<MetaAddr>) -> Self {
        Self {
            recently_live_peers,
        }
    }
}

impl AddressBookPeers for MockAddressBookPeers {
    fn recently_live_peers(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<MetaAddr> {
        self.recently_live_peers
            .iter()
            .filter(|peer| peer.was_recently_live(now))
            .cloned()
            .collect()
    }
}
