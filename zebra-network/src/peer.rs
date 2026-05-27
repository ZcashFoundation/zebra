//! Peer connection handling.

mod client;
mod connection;
mod connector;
mod error;
mod handshake;
mod load_tracked_client;
mod minimum_peer_version;
mod priority;

#[cfg(any(test, feature = "proptest-impl"))]
#[allow(unused_imports)]
pub use client::tests::ClientTestHarness;

#[cfg(test)]
pub(crate) use client::tests::ReceiveRequestAttempt;
#[cfg(test)]
pub(crate) use handshake::register_inventory_status;

use client::{ClientRequestReceiver, InProgressClientRequest, MustUseClientResponseSender};

pub(crate) use client::{CancelHeartbeatTask, ClientRequest};

pub use client::Client;
pub use connection::Connection;
pub use connector::{Connector, OutboundConnectorRequest};
pub use error::{ErrorSlot, HandshakeError, PeerError, SharedPeerError};
pub use handshake::{ConnectedAddr, ConnectionInfo, Handshake, HandshakeRequest};
pub use load_tracked_client::LoadTrackedClient;
pub use minimum_peer_version::MinimumPeerVersion;
#[allow(unused_imports)]
pub use priority::{
    address_is_valid_for_inbound_listeners, address_is_valid_for_outbound_connections,
    AttributePreference, PeerPreference,
};

/// Low-cardinality source of a peer address for metrics labels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PeerSource {
    /// Peers resolved from `dnsseed.str4d.xyz`.
    DnsSeedStr4d,
    /// Peers resolved from `dnsseed.z.cash` or testnet `z.cash` seeders.
    DnsSeedZcash,
    /// Peers resolved from Shielded Infrastructure seeders.
    DnsSeedShieldedInfra,
    /// Peers resolved from Zcash Foundation seeders.
    DnsSeedZfnd,
    /// Peers loaded from Zebra's disk peer cache.
    PeerCache,
    /// Peers learned from peer gossip or address responses.
    Gossip,
    /// Peers configured explicitly by the node operator.
    Configured,
    /// Peers accepted through Zebra's inbound listener.
    Inbound,
    /// Isolated peers created without address-book metadata.
    Isolated,
}

impl PeerSource {
    /// Returns the bounded label used in Prometheus metrics.
    pub fn label(self) -> &'static str {
        match self {
            PeerSource::DnsSeedStr4d => "dnsseed_str4d",
            PeerSource::DnsSeedZcash => "dnsseed_zcash",
            PeerSource::DnsSeedShieldedInfra => "dnsseed_shieldedinfra",
            PeerSource::DnsSeedZfnd => "dnsseed_zfnd",
            PeerSource::PeerCache => "peer_cache",
            PeerSource::Gossip => "gossip",
            PeerSource::Configured => "configured",
            PeerSource::Inbound => "inbound",
            PeerSource::Isolated => "isolated",
        }
    }

    /// Returns the source label for a configured initial peer entry.
    pub fn from_initial_peer_host(host: &str) -> Self {
        if host.contains("str4d") {
            PeerSource::DnsSeedStr4d
        } else if host.contains("z.cash") {
            PeerSource::DnsSeedZcash
        } else if host.contains("shieldedinfra") {
            PeerSource::DnsSeedShieldedInfra
        } else if host.contains("zfnd") {
            PeerSource::DnsSeedZfnd
        } else {
            PeerSource::Configured
        }
    }
}

/// Returns a low-cardinality user-agent family for metrics labels.
pub fn user_agent_family(user_agent: &str) -> &'static str {
    if user_agent.contains("Zebra") {
        "zebra"
    } else if user_agent.contains("MagicBean") || user_agent.contains("zcashd") {
        "zcashd"
    } else if user_agent.is_empty() {
        "unknown"
    } else {
        "other"
    }
}
