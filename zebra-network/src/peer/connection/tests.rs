//! Tests for peer connections

use std::io;

use futures::{channel::mpsc, sink::SinkMapErr, SinkExt};

use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::MockService;

use crate::{
    peer::{ClientRequest, ConnectedAddr, Connection, ErrorSlot},
    peer_set::ActiveConnectionCounter,
    protocol::external::Message,
    Request, Response,
};

mod prop;
mod vectors;

/// Creates a new [`Connection`] instance for testing.
fn new_test_connection<A>() -> (
    Connection<
        MockService<Request, Response, A>,
        SinkMapErr<mpsc::UnboundedSender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, A>,
    mpsc::UnboundedReceiver<Message>,
    ErrorSlot,
) {
    let mock_inbound_service = MockService::build().finish();
    let (client_tx, client_rx) = mpsc::channel(1);
    let shared_error_slot = ErrorSlot::default();
    let (peer_outbound_tx, peer_outbound_rx) = mpsc::unbounded();

    let error_converter: fn(mpsc::SendError) -> SerializationError = |_| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "peer outbound message stream was closed",
        )
        .into()
    };
    let peer_tx = peer_outbound_tx.sink_map_err(error_converter);

    let connection = Connection::new(
        mock_inbound_service.clone(),
        client_rx,
        shared_error_slot.clone(),
        peer_tx,
        ActiveConnectionCounter::new_counter().track_connection(),
        ConnectedAddr::Isolated,
    );

    (
        connection,
        client_tx,
        mock_inbound_service,
        peer_outbound_rx,
        shared_error_slot,
    )
}
