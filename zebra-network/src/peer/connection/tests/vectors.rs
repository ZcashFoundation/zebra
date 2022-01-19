//! Fixed test vectors for peer connections.
//!
//! TODO:
//! - connection tests when awaiting requests (#3232)
//! - connection tests with closed/dropped peer_outbound_tx (#3233)

use futures::{channel::mpsc, FutureExt};
use tokio_util::codec::FramedWrite;

use zebra_chain::parameters::Network;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    peer::{
        client::ClientRequestReceiver, connection::State, ClientRequest, Connection, ErrorSlot,
    },
    peer_set::ActiveConnectionCounter,
    protocol::external::Codec,
    PeerError, Request, Response,
};

#[tokio::test]
async fn connection_run_loop_ok() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, client_tx, shared_error_slot) = new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // The run loop will wait forever for a request from Zebra or the peer,
    // without any errors, channel closes, or bytes written.
    //
    // But the connection closes if we drop the future, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert!(
        matches!(error, None),
        "unexpected connection error: {:?}",
        error
    );

    assert!(!client_tx.is_closed());
    assert!(!peer_inbound_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_future_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, client_tx, shared_error_slot) = new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // now_or_never implicitly drops the connection future.
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());
    assert!(peer_inbound_tx.is_closed());

    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_client_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, mut client_tx, shared_error_slot) =
        new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // Explicitly close the client channel.
    client_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());
    assert!(peer_inbound_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_client_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, client_tx, shared_error_slot) = new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // Drop the client channel.
    std::mem::drop(client_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(peer_inbound_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_inbound_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, client_tx, shared_error_slot) = new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // Explicitly close the inbound peer channel.
    peer_inbound_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());
    assert!(peer_inbound_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_inbound_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (connection, client_tx, shared_error_slot) = new_test_connection(&mut peer_outbound_bytes);

    let connection = connection.run(peer_inbound_rx);

    // Drop the inbound peer channel.
    std::mem::drop(peer_inbound_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

#[tokio::test]
async fn connection_run_loop_failed() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let mut peer_outbound_bytes = Vec::<u8>::new();

    let (mut connection, client_tx, shared_error_slot) =
        new_test_connection(&mut peer_outbound_bytes);

    // Simulate an internal connection error.
    connection.state = State::Failed;
    shared_error_slot
        .try_update_error(PeerError::ClientRequestTimeout.into())
        .expect("unexpected previous error in tests");

    let connection = connection.run(peer_inbound_rx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    // Because the peer error mutex is a sync mutex,
    // the connection can't exit until it reaches the outer async loop.
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());
    assert!(peer_inbound_tx.is_closed());

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    assert_eq!(peer_outbound_bytes, Vec::<u8>::new());
}

/// Creates a new [`Connection`] instance for testing.
fn new_test_connection(
    peer_outbound_bytes: &mut Vec<u8>,
) -> (
    Connection<MockService<Request, Response, PanicAssertion>, FramedWrite<&mut Vec<u8>, Codec>>,
    mpsc::Sender<ClientRequest>,
    ErrorSlot,
) {
    let (client_tx, client_rx) = mpsc::channel(1);

    let peer_outbound_tx = FramedWrite::new(
        peer_outbound_bytes,
        Codec::builder()
            .for_network(Network::Mainnet)
            .with_metrics_addr_label("test".into())
            .finish(),
    );

    let unused_inbound_service = MockService::build().for_unit_tests();

    let shared_error_slot = ErrorSlot::default();

    let connection = Connection {
        state: State::AwaitingRequest,
        request_timer: None,
        cached_addrs: Vec::new(),
        svc: unused_inbound_service,
        client_rx: ClientRequestReceiver::from(client_rx),
        error_slot: shared_error_slot.clone(),
        peer_tx: peer_outbound_tx,
        connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
        metrics_label: "test".to_string(),
        last_metrics_state: None,
    };

    (connection, client_tx, shared_error_slot)
}
