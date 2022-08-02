//! Tests for peer connections

use std::io;

use futures::{channel::mpsc, sink::SinkMapErr, SinkExt};

use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::MockService;

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{ClientRequest, ConnectedAddr, Connection, ConnectionInfo, ErrorSlot},
    peer_set::ActiveConnectionCounter,
    protocol::{external::Message, types::PeerServices},
    Request, Response,
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

    let connection_info = ConnectionInfo {
        remote_version: CURRENT_NETWORK_PROTOCOL_VERSION,
        negotiated_version: CURRENT_NETWORK_PROTOCOL_VERSION,
        connected_addr: ConnectedAddr::Isolated,
        peer_services: PeerServices::default(),
        user_agent: "connection tests".to_string(),
    };

    let connection = Connection::new(
        mock_inbound_service.clone(),
        client_rx,
        shared_error_slot.clone(),
        peer_tx,
        ActiveConnectionCounter::new_counter().track_connection(),
        connection_info,
    );

    (
        connection,
        client_tx,
        mock_inbound_service,
        peer_rx,
        shared_error_slot,
    )
}
