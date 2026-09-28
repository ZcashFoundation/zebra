//! A AddressBookPeers trait for getting the [`MetaAddr`] of recently live peers.

use chrono::Utc;

use crate::{meta_addr::MetaAddr, PeerSocketAddr};

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;

#[cfg(any(test, feature = "proptest-impl"))]
pub use mock::MockAddressBookPeers;

/// Method signatures for getting [`MetaAddr`]s of recently live peers.
pub trait AddressBookPeers {
    /// Return an Vec of peers we've seen recently, in reconnection attempt order.
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr>;

    /// Add a peer to the address book.
    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool;

    /// Returns the misbehavior score for the peer group containing `addr`:
    /// [`MAX_PEER_MISBEHAVIOR_SCORE`](crate::constants::MAX_PEER_MISBEHAVIOR_SCORE)
    /// if the group is banned, and `0` otherwise.
    ///
    /// Bans apply per peer group — one IPv4 address, or one IPv6 `/64` subnet —
    /// so every address in a group reports the same score.
    ///
    /// Defaults to `0` for implementors that don't track misbehavior.
    fn misbehavior_score(&self, _addr: PeerSocketAddr) -> u32 {
        0
    }
}
