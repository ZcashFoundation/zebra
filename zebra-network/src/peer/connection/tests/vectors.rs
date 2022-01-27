//! Fixed test vectors for peer connections.
//!
//! TODO:
//! - connection tests when awaiting requests (#3232)
//! - connection tests with closed/dropped peer_outbound_tx (#3233)

use std::{task::Poll, time::Duration};

use futures::{
    channel::{mpsc, oneshot},
    sink::SinkMapErr,
    FutureExt, StreamExt,
};

use tracing::Span;
use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    constants::REQUEST_TIMEOUT,
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
async fn connection_run_loop_spawn_ok() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_inbound_rx));

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none());

    assert!(!client_tx.is_closed());
    assert!(!peer_inbound_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(matches!(connection_result, Poll::Pending));

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();

    assert!(peer_outbound_messages.next().await.is_none());
}

#[tokio::test]
async fn connection_run_loop_message_ok() {
    zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_inbound_tx, peer_inbound_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_inbound_rx));

    // Simulate a message send and receive
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        span: Span::current(),
    };

    client_tx
        .try_send(request)
        .expect("internal request channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert!(matches!(outbound_message, Some(Message::GetAddr)));

    peer_inbound_tx
        .try_send(Ok(Message::Addr(Vec::new())))
        .expect("peer inbound response channel is valid");

    // give the event loop time to run
    tokio::task::yield_now().await;
    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect("response is a message (not an error)"),
        Response::Peers(Vec::new()),
    );

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none());

    assert!(!client_tx.is_closed());
    assert!(!peer_inbound_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(matches!(connection_result, Poll::Pending));

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();

    assert!(peer_outbound_messages.next().await.is_none());
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
        .try_update_error(PeerError::ClientReceiveTimeout.into())
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

#[tokio::test]
async fn connection_run_loop_send_timeout() {
    zebra_test::init();

    tokio::time::pause();

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

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_inbound_rx));

    // Simulate a message send timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // Make the send timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Timeouts don't close the connection
    let error = shared_error_slot.try_get_error();
    assert!(error.is_none());

    assert!(!client_tx.is_closed());
    assert!(!peer_inbound_tx.is_closed());

    let outbound_message = peer_outbound_messages.next().await;
    assert!(matches!(outbound_message, Some(Message::GetAddr)));

    // TODO: check for PeerError::ClientSendTimeout
    let peer_response = request_rx.try_recv();
    assert!(matches!(peer_response, Ok(Some(Err(_)))));

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(matches!(connection_result, Poll::Pending));

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();

    assert!(peer_outbound_messages.next().await.is_none());
}

#[tokio::test]
async fn connection_run_loop_receive_timeout() {
    zebra_test::init();

    tokio::time::pause();

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

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_inbound_rx));

    // Simulate a message receive timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert!(matches!(outbound_message, Some(Message::GetAddr)));

    // Make the receive timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Timeouts don't close the connection
    let error = shared_error_slot.try_get_error();
    assert!(error.is_none());

    assert!(!client_tx.is_closed());
    assert!(!peer_inbound_tx.is_closed());

    // TODO: check for PeerError::ClientReceiveTimeout
    let peer_response = request_rx.try_recv();
    assert!(matches!(peer_response, Ok(Some(Err(_)))));

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(matches!(connection_result, Poll::Pending));

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();

    assert!(peer_outbound_messages.next().await.is_none());
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
