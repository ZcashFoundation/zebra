//! Tests for peer connections

#![allow(clippy::unwrap_in_result)]

use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use chrono::Utc;
use futures::{channel::mpsc, sink::SinkMapErr, SinkExt};

use zebra_chain::{block::Height, serialization::SerializationError};
use zebra_test::mock_service::MockService;

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientRequest, ConnectedAddr, Connection, ConnectionInfo, ErrorSlot},
    peer_set::ActiveConnectionCounter,
    protocol::{
        external::{AddrInVersion, Message},
        types::{Nonce, PeerServices},
    },
    Request, Response, VersionMessage,
};

mod prop;
mod vectors;

/// Creates a new [`Connection`] instance for testing.
fn new_test_connection<A>() -> (
    Connection<
        MockService<Request, Response, A>,
        SinkMapErr<mpsc::Sender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, A>,
    mpsc::Receiver<Message>,
    ErrorSlot,
) {
    let mock_inbound_service = MockService::build().finish();
    let (client_tx, client_rx) = mpsc::channel(0);
    let shared_error_slot = ErrorSlot::default();

    // Normally the network has more capacity than the sender's single implicit slot,
    // but the smaller capacity makes some tests easier.
    let (peer_tx, peer_rx) = mpsc::channel(0);

    let error_converter: fn(mpsc::SendError) -> SerializationError = |_| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "peer outbound message stream was closed",
        )
        .into()
    };
    let peer_tx = peer_tx.sink_map_err(error_converter);

    let fake_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4).into();
    let fake_version = CURRENT_NETWORK_PROTOCOL_VERSION;
    let fake_services = PeerServices::default();

    let remote = VersionMessage {
        version: fake_version,
        services: fake_services,
        timestamp: Utc::now(),
        address_recv: AddrInVersion::new(fake_addr, fake_services),
        address_from: AddrInVersion::new(fake_addr, fake_services),
        nonce: Nonce::default(),
        user_agent: "connection test".to_string(),
        start_height: Height(0),
        relay: true,
    };

    let connection_info = ConnectionInfo {
        connected_addr: ConnectedAddr::Isolated,
        remote,
        negotiated_version: fake_version,
    };

    let connection = Connection::new(
        mock_inbound_service.clone(),
        client_rx,
        shared_error_slot.clone(),
        peer_tx,
        ActiveConnectionCounter::new_counter().track_connection(),
        Arc::new(connection_info),
        Vec::new(),
    );

    (
        connection,
        client_tx,
        mock_inbound_service,
        peer_rx,
        shared_error_slot,
    )
}
