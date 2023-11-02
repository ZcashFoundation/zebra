//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    pub addr: PeerSocketAddr,

    /// The internal state of the peer.
    pub state: String,

    /// The untrusted remote last seen time of the peer.
    pub remote_last_seen: String,

    /// The last response time from the peer.
    ///
    /// If this was in seconds since the epoch, it would be the specified `lastrecv` field.
    pub last_response: String,

    /// The last failure time from the peer.
    pub last_failed: String,
}

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        Self {
            addr: meta_addr.addr(),
            state: format!("{:?}", meta_addr.last_connection_state()),
            remote_last_seen: format!("{:?}", meta_addr.untrusted_last_seen()),
            last_response: format!("{:?}", meta_addr.last_response()),
            last_failed: format!("{:?}", meta_addr.last_failure()),
        }
    }
}
