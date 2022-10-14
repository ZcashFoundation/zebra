//! Acceptance tests for zebra-network APIs.

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use chrono::Utc;

use zebra_chain::block::Height;
use zebra_network::{
    types::{AddrInVersion, Nonce, PeerServices},
    ConnectedAddr, ConnectionInfo, Version, VersionMessage,
};

/// Test that the types used in [`ConnectionInfo`] are public,
/// by compiling code that explicitly uses those types.
#[test]
fn connection_info_types_are_public() {
    let fake_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 3).into();
    let fake_version = Version(3);
    let fake_services = PeerServices::default();

    // Each struct field must have its type explicitly listed here
    let connected_addr: ConnectedAddr = ConnectedAddr::OutboundDirect { addr: fake_addr };
    let negotiated_version: Version = fake_version;

    let remote = VersionMessage {
        version: fake_version,
        services: fake_services,
        timestamp: Utc::now(),
        address_recv: AddrInVersion::new(fake_addr, fake_services),
        address_from: AddrInVersion::new(fake_addr, fake_services),
        nonce: Nonce::default(),
        user_agent: "public API compile test".to_string(),
        start_height: Height(0),
        relay: true,
    };

    let _connection_info = Arc::new(ConnectionInfo {
        connected_addr,
        remote,
        negotiated_version,
    });
}
