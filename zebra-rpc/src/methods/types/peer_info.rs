//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    pub addr: PeerSocketAddr,
}

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        Self {
            addr: meta_addr.addr(),
        }
    }
}

impl Default for PeerInfo {
    fn default() -> Self {
        Self {
            addr: PeerSocketAddr::unspecified(),
        }
    }
}
