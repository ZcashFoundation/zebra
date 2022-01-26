//! Fixed test vectors for peer connections.
//!
//! TODO:
//! - connection tests when awaiting requests (#3232)
//! - connection tests with closed/dropped peer_outbound_tx (#3233)

use futures::{channel::mpsc, sink::SinkMapErr, FutureExt, StreamExt};

use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    peer::{
        connection::{Connection, State},
        ClientRequest, ErrorSlot,
    },
    protocol::external::Message,
    PeerError, Request, Response,
};

#[tokio::test]
async fn connection_run_loop_ok() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_future_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_inbound_rx);

    // now_or_never implicitly drops the connection future.
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert!(matches!(error, Some(_)));

    assert!(client_tx.is_closed());
    assert!(peer_inbound_tx.is_closed());

    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_client_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_client_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_inbound_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_inbound_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

#[tokio::test]
async fn connection_run_loop_failed() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (
        mut connection,
        client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

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
    assert!(peer_outbound_messages.next().await.is_none());

    inbound_service.expect_no_requests().await;
}

/// Creates a new [`Connection`] instance for unit tests.
fn new_test_connection() -> (
    Connection<
        MockService<Request, Response, PanicAssertion>,
        SinkMapErr<mpsc::UnboundedSender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, PanicAssertion>,
    mpsc::UnboundedReceiver<Message>,
    ErrorSlot,
) {
    super::new_test_connection()
}
