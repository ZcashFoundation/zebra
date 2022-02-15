//! Fixed test vectors for peer connections.
//!
//! TODO: add tests for:
//!   - inbound message as request
//!   - inbound message, but not a request (or a response)

use std::{collections::HashSet, task::Poll, time::Duration};

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

/// Test that the connection run loop works as a future
#[tokio::test]
async fn connection_run_loop_ok() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // The run loop will wait forever for a request from Zebra or the peer,
    // without any errors, channel closes, or bytes written.
    //
    // But the connection closes if we drop the future, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {:?}", error);

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop works as a spawned task
#[tokio::test]
async fn connection_run_loop_spawn_ok() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {:?}", error);

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {:?}",
        connection_result,
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop works as a spawned task with messages in and out
#[tokio::test]
async fn connection_run_loop_message_ok() {
    zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send and receive
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx
        .try_send(request)
        .expect("internal request channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    peer_tx
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
    assert!(error.is_none(), "unexpected error: {:?}", error);

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {:?}",
        connection_result,
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when dropped
#[tokio::test]
async fn connection_run_loop_future_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // now_or_never implicitly drops the connection future.
    let result = connection.now_or_never();
    assert_eq!(result, None);

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionDropped",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the internal client closes the connection channel
#[tokio::test]
async fn connection_run_loop_client_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    let connection = connection.run(peer_rx);

    // Explicitly close the client channel.
    client_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ClientDropped",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the internal client drops the connection channel
#[tokio::test]
async fn connection_run_loop_client_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Drop the client channel.
    std::mem::drop(client_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ClientDropped",
    );

    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the peer channel is closed.
/// (We're not sure if tokio closes or drops the TcpStream when the TCP connection closes.)
#[tokio::test]
async fn connection_run_loop_inbound_close() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (mut peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Explicitly close the inbound peer channel.
    peer_tx.close_channel();

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionClosed",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when the peer channel is dropped
/// (We're not sure if tokio closes or drops the TcpStream when the TCP connection closes.)
#[tokio::test]
async fn connection_run_loop_inbound_drop() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    let connection = connection.run(peer_rx);

    // Drop the inbound peer channel.
    std::mem::drop(peer_tx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionClosed",
    );

    assert!(client_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly on internal connection errors.
#[tokio::test]
async fn connection_run_loop_failed() {
    zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

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
        .try_update_error(PeerError::Overloaded.into())
        .expect("unexpected previous error in tests");

    let connection = connection.run(peer_rx);

    // If we drop the future, the connection will close anyway, so we avoid the drop by cloning it.
    let connection = connection.shared();
    let connection_guard = connection.clone();
    let result = connection.now_or_never();
    // Because the peer error mutex is a sync mutex,
    // the connection can't exit until it reaches the outer async loop.
    assert_eq!(result, Some(()));

    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "Overloaded",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // We need to drop the future, because it holds a mutable reference to the bytes.
    std::mem::drop(connection_guard);
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when sending a message to a peer times out,
/// but we are not expecting a response message from the peer.
#[tokio::test]
async fn connection_run_loop_send_timeout_nil_response() {
    zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::AdvertiseTransactionIds(HashSet::new()),
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // Make the send timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Send timeouts close the connection
    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionSendTimeout",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::Inv(Vec::new())));

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionSendTimeout",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection finished, but did not panic.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Ready(Ok(()))),
        "expected run loop termination, but run loop continued: {:?}",
        connection_result,
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when sending a message to a peer times out,
/// and we are expecting a response message from the peer.
#[tokio::test]
async fn connection_run_loop_send_timeout_expect_response() {
    zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message send timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");

    // Make the send timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Send timeouts close the connection
    let error = shared_error_slot.try_get_error();
    assert_eq!(
        error.expect("missing expected error").inner_debug(),
        "ConnectionSendTimeout",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionSendTimeout",
    );

    assert!(client_tx.is_closed());
    assert!(peer_tx.is_closed());

    inbound_service.expect_no_requests().await;

    // Make sure that the connection finished, but did not panic.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Ready(Ok(()))),
        "expected run loop termination, but run loop continued: {:?}",
        connection_result,
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop continues but returns an error to the client,
/// when a peer accepts a message, but does not send an expected response.
#[tokio::test]
async fn connection_run_loop_receive_timeout() {
    zebra_test::init();

    tokio::time::pause();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        mut inbound_service,
        mut peer_outbound_messages,
        shared_error_slot,
    ) = new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    // Simulate a message receive timeout
    let (request_tx, mut request_rx) = oneshot::channel();
    let request = ClientRequest {
        request: Request::Peers,
        tx: request_tx,
        inv_collector: None,
        transient_addr: None,
        span: Span::current(),
    };

    client_tx.try_send(request).expect("channel is valid");
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, Some(Message::GetAddr));

    // Make the receive timeout
    tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;

    // Receive timeouts don't close the connection
    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {:?}", error);

    assert!(!client_tx.is_closed());
    assert!(!peer_tx.is_closed());

    let peer_response = request_rx.try_recv();
    assert_eq!(
        peer_response
            .expect("peer internal response channel is valid")
            .expect("response is present")
            .expect_err("response is an error (not a message)")
            .inner_debug(),
        "ConnectionReceiveTimeout",
    );

    inbound_service.expect_no_requests().await;

    // Make sure that the connection did not:
    // - panic, or
    // - return.
    //
    // This test doesn't cause any fatal errors,
    // so returning would be incorrect behaviour.
    let connection_result = futures::poll!(&mut connection_join_handle);
    assert!(
        matches!(connection_result, Poll::Pending),
        "unexpected run loop termination: {:?}",
        connection_result,
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Creates a new [`Connection`] instance for unit tests.
fn new_test_connection() -> (
    Connection<
        MockService<Request, Response, PanicAssertion>,
        SinkMapErr<mpsc::Sender<Message>, fn(mpsc::SendError) -> SerializationError>,
    >,
    mpsc::Sender<ClientRequest>,
    MockService<Request, Response, PanicAssertion>,
    mpsc::Receiver<Message>,
    ErrorSlot,
) {
    super::new_test_connection()
}
