//! Tests for peer connections

use std::io;

use futures::{channel::mpsc, sink::SinkMapErr, SinkExt};

use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::MockService;

use crate::{
    peer::{
        client::ClientRequestReceiver, connection::State, ClientRequest, Connection, ErrorSlot,
    },
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

    let connection = Connection {
        state: State::AwaitingRequest,
        request_timer: None,
        cached_addrs: Vec::new(),
        svc: mock_inbound_service.clone(),
        client_rx: ClientRequestReceiver::from(client_rx),
        error_slot: shared_error_slot.clone(),
        peer_tx,
        connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
        metrics_label: "test".to_string(),
        last_metrics_state: None,
    };

    (
        connection,
        client_tx,
        mock_inbound_service,
        peer_outbound_rx,
        shared_error_slot,
    )
}
