//! Shared helpers for the version 2 loopback tests.

use std::{net::SocketAddr, time::Duration};

use quinn::{Connection, Endpoint};
use tokio::time::Instant;

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    peer::{ConnectedAddr, MinimumPeerVersion},
    protocol::external::types::PeerServices,
    quic,
};

use super::{
    handshake::{handshake, CompletedHandshake, HandshakeError, LocalIdentity, Role},
    nonces::NonceRegistry,
};

/// The handshake deadline used by the tests.
pub(super) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// A connected pair of loopback QUIC connections on `network`, with the
/// endpoints kept alive.
pub(super) async fn connected_pair(
    network: &Network,
) -> (Connection, Connection, Endpoint, Endpoint) {
    let server = quic::bind(network, "127.0.0.1:0".parse().unwrap()).expect("binds");
    let client = quic::bind(network, "127.0.0.1:0".parse().unwrap()).expect("binds");
    let server_addr = server.local_addr().expect("bound");

    let (outbound, inbound) = tokio::join!(quic::connect(&client, server_addr), async {
        server
            .accept()
            .await
            .expect("open")
            .accept()
            .expect("acceptable")
            .await
    });
    (
        outbound.expect("connects"),
        inbound.expect("accepts"),
        client,
        server,
    )
}

/// The identity a test node presents.
pub(super) fn identity(user_agent: &str) -> LocalIdentity {
    LocalIdentity {
        user_agent: user_agent.to_string(),
        services: PeerServices::NODE_NETWORK,
        relay: true,
    }
}

/// Runs the handshake as `role` on `connection`.
pub(super) async fn run_handshake(
    connection: &Connection,
    role: Role,
    network: &Network,
    identity: &LocalIdentity,
    nonces: &NonceRegistry,
) -> Result<CompletedHandshake, HandshakeError> {
    let addr: SocketAddr = connection.remote_address();
    let connected_addr = match role {
        Role::Initiator => ConnectedAddr::new_outbound_direct(addr.into()),
        Role::Responder => ConnectedAddr::new_inbound_direct(addr.into()),
    };
    let mut minimum_peer_version = MinimumPeerVersion::new(NoChainTip, network);
    handshake(
        connection,
        connected_addr,
        role,
        identity,
        &mut minimum_peer_version,
        nonces,
        Instant::now() + HANDSHAKE_DEADLINE,
    )
    .await
}
