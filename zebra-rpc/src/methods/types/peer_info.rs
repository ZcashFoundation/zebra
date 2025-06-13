//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use derive_getters::Getters;
use derive_new::new;
use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    #[getter(copy)]
    pub(crate) addr: PeerSocketAddr,

    /// Inbound (true) or Outbound (false)
    pub(crate) inbound: bool,
}

/// Response type for the `getpeerinfo` RPC method.
pub type GetPeerInfoResponse = Vec<PeerInfo>;

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        Self {
            addr: meta_addr.addr(),
            inbound: meta_addr.is_inbound(),
        }
    }
}

impl Default for PeerInfo {
    fn default() -> Self {
        Self {
            addr: PeerSocketAddr::unspecified(),
            inbound: false,
        }
    }
}
