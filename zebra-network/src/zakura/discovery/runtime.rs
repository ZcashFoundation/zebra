//! Wiring helpers for constructing the native discovery runtime handle.

use std::net::SocketAddr;

use iroh::SecretKey;
use tokio::sync::watch;

use crate::zakura::{ZakuraHandshakeConfig, ZakuraPeerId};

use super::protocol::{
    DiscoveryWireError, ZakuraDiscoveryConfig, ZakuraDiscoveryHandle, ZakuraDiscoveryLocalConfig,
    ZakuraServiceId, DEFAULT_DISCOVERY_CONNECTION_HEADROOM,
};

/// Services advertised in this node's discovery self-record.
pub(crate) fn default_advertised_services() -> Vec<ZakuraServiceId> {
    vec![
        ZakuraServiceId::discovery(),
        ZakuraServiceId::block_sync(),
        ZakuraServiceId::header_sync(),
        ZakuraServiceId::legacy_gossip(),
        ZakuraServiceId::legacy_requests(),
        ZakuraServiceId::service_discovery(),
    ]
}

/// Reserves at least one connection slot per configured bootstrap peer so
/// bootstrap dials are never starved by discovered-peer dials.
pub(crate) fn effective_discovery_connection_headroom(bootstrap_peer_count: usize) -> usize {
    DEFAULT_DISCOVERY_CONNECTION_HEADROOM.max(bootstrap_peer_count)
}

/// Builds a discovery runtime handle from the local node identity and the
/// negotiated network parameters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_discovery_handle(
    secret_key: SecretKey,
    direct_addrs: Vec<SocketAddr>,
    advertised_services: Vec<ZakuraServiceId>,
    handshake: &ZakuraHandshakeConfig,
    max_zakura_connections: usize,
    bootstrap_peer_count: usize,
    connected: watch::Receiver<Vec<ZakuraPeerId>>,
) -> Result<ZakuraDiscoveryHandle, DiscoveryWireError> {
    let local_config = ZakuraDiscoveryLocalConfig {
        secret_key,
        direct_addrs,
        services: advertised_services,
        zakura_protocol_min: handshake.zakura_protocol_min,
        zakura_protocol_max: handshake.zakura_protocol_max,
        network_id: handshake.network_id,
        chain_id: handshake.chain_id,
        last_authored_sequence: None,
    };
    let config = ZakuraDiscoveryConfig {
        max_zakura_connections,
        discovery_connection_headroom: effective_discovery_connection_headroom(
            bootstrap_peer_count,
        ),
        ..ZakuraDiscoveryConfig::default()
    };
    ZakuraDiscoveryHandle::new(local_config, config, connected)
}
