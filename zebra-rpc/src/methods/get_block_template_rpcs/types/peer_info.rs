//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use zebra_network::types::MetaAddr;

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    addr: String,
}

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        Self {
            addr: meta_addr.addr().to_string(),
        }
    }
}
