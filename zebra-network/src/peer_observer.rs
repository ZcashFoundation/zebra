//! A PeerObserver trait for getting the [`MetaAddr`] of recently live peers.

use chrono::Utc;

use crate::meta_addr::MetaAddr;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;

#[cfg(any(test, feature = "proptest-impl"))]
pub use mock::MockPeerObserver;

/// Method signatures for getting [`MetaAddr`]s of recently live peers.
pub trait PeerObserver {
    /// Return an Vec of peers we've seen recently, in reconnection attempt order.
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr>;
}
