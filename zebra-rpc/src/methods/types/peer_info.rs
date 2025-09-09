//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use derive_getters::Getters;
use derive_new::new;
use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    #[getter(copy)]
    pub(crate) addr: PeerSocketAddr,

    /// Inbound (true) or Outbound (false)
    pub(crate) inbound: bool,

    /// The round-trip ping time in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pingtime: Option<f64>,

    /// The wait time on a ping response in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pingwait: Option<f64>,
}

/// Response type for the `getpeerinfo` RPC method.
pub type GetPeerInfoResponse = Vec<PeerInfo>;

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        Self {
            addr: meta_addr.addr(),
            inbound: meta_addr.is_inbound(),
            // TODO: Fill in pingtime and pingwait once live ping tracking is implemented.
            pingtime: None,
            pingwait: None,
        }
    }
}

impl Default for PeerInfo {
    fn default() -> Self {
        Self {
            addr: PeerSocketAddr::unspecified(),
            inbound: false,
            pingtime: None,
            pingwait: None,
        }
    }
}
