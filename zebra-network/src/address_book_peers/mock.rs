//! Mock [`AddressBookPeers`] for use in tests.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{meta_addr::MetaAddr, AddressBookPeers, PeerSocketAddr};

/// A mock [`AddressBookPeers`] implementation that's always empty.
#[derive(Debug, Default, Clone)]
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

    /// Adds a peer to the mock address book.
    pub fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        // The real add peer will use `MetaAddr::new_initial_peer` but we just want to get a `MetaAddr` for the mock.
        let rtt = Duration::from_millis(100);
        self.recently_live_peers.push(
            MetaAddr::new_responded(peer, Some(rtt), None).into_new_meta_addr(
                Instant::now(),
                chrono::Utc::now()
                    .try_into()
                    .expect("will succeed until 2038"),
            ),
        );
        true
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

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        self.add_peer(peer)
    }
}

/// A shared version of [`MockAddressBookPeers`] that can be used in shared state tests.
pub struct SharedMockAddressBookPeers {
    inner: Arc<Mutex<MockAddressBookPeers>>,
}

impl SharedMockAddressBookPeers {
    /// Creates a new instance of [`SharedMockAddressBookPeers`].
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockAddressBookPeers::default())),
        }
    }
}

impl Default for SharedMockAddressBookPeers {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressBookPeers for SharedMockAddressBookPeers {
    fn recently_live_peers(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<MetaAddr> {
        self.inner.lock().unwrap().recently_live_peers(now)
    }

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        self.inner.lock().unwrap().add_peer(peer)
    }
}

impl Clone for SharedMockAddressBookPeers {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
