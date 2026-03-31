//! An array of [`PeerInfo`] is the output of the `getpeerinfo` RPC method.

use derive_getters::Getters;
use derive_new::new;
use zebra_network::{types::MetaAddr, PeerSocketAddr};

/// Item of the `getpeerinfo` response
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
#[allow(clippy::too_many_arguments)]
pub struct PeerInfo {
    /// The IP address and port of the peer
    #[getter(copy)]
    pub(crate) addr: PeerSocketAddr,

    /// The services offered, as a zero-padded 16-character hex string
    pub(crate) services: String,

    /// The time in seconds since epoch of the last message received from this peer
    pub(crate) lastrecv: u32,

    /// Inbound (true) or Outbound (false)
    pub(crate) inbound: bool,

    /// The ban score for misbehavior
    pub(crate) banscore: u32,

    /// The peer's user agent string (e.g. "/Zebra:2.1.0/" or "/MagicBean:5.8.0/")
    pub(crate) subver: String,

    /// The negotiated protocol version
    pub(crate) version: u32,

    /// The connection state (e.g. "Responded", "NeverAttemptedGossiped", "Failed", "AttemptPending")
    pub(crate) connection_state: String,

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
        let services = meta_addr
            .services()
            .map(|s| format!("{:016x}", s.bits()))
            .unwrap_or_else(|| "0000000000000000".to_string());

        let lastrecv = meta_addr.last_seen().map(|t| t.timestamp()).unwrap_or(0);

        let connection_state = format!("{:?}", meta_addr.last_connection_state());

        let subver = meta_addr.user_agent().unwrap_or("").to_string();

        let version = meta_addr.negotiated_version().map(|v| v.0).unwrap_or(0);

        Self {
            addr: meta_addr.addr(),
            services,
            lastrecv,
            inbound: meta_addr.is_inbound(),
            banscore: meta_addr.misbehavior(),
            subver,
            version,
            connection_state,
            pingtime: meta_addr.rtt().map(|d| d.as_secs_f64()),
            pingwait: meta_addr.ping_sent_at().map(|t| t.elapsed().as_secs_f64()),
        }
    }
}

impl Default for PeerInfo {
    fn default() -> Self {
        Self {
            addr: PeerSocketAddr::unspecified(),
            services: "0000000000000000".to_string(),
            lastrecv: 0,
            inbound: false,
            banscore: 0,
            subver: String::new(),
            version: 0,
            connection_state: String::new(),
            pingtime: None,
            pingwait: None,
        }
    }
}
