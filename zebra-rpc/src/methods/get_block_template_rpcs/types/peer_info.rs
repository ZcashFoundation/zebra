//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    pub addr: PeerSocketAddr,

    /// The internal state of the peer.
    pub state: String,

    /// The negotiated connection version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negotiated_version: Option<String>,

    /// The last version message sent by the peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_version_message: Option<String>,

    /// The untrusted remote last seen time of the peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_last_seen: Option<String>,

    /// The last response time from the peer.
    ///
    /// If this was in seconds since the epoch, it would be the specified `lastrecv` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response: Option<String>,

    /// The last failure time from the peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
}

impl From<MetaAddr> for PeerInfo {
    fn from(meta_addr: MetaAddr) -> Self {
        let info = meta_addr.last_connection_info();

        Self {
            addr: meta_addr.addr(),
            state: format!("{:?}", meta_addr.last_connection_state()),
            negotiated_version: info
                .as_ref()
                .and_then(|info| Some(format!("{:?}", info.negotiated_version?))),
            last_version_message: info.as_ref().map(|info| format!("{:?}", info.remote)),
            remote_last_seen: meta_addr
                .untrusted_last_seen()
                .map(|time| format!("{:?}", time)),
            last_response: meta_addr.last_response().map(|time| format!("{:?}", time)),
            last_failure: meta_addr
                .last_failure()
                .map(|instant| format!("{:?}", instant)),
        }
    }
}
