//! Fixed test vectors for peer connections.
//!
//! TODO: add tests for:
//!   - inbound message as request
//!   - inbound message, but not a request (or a response)

use std::{
    collections::HashSet,
    task::Poll,
    time::{Duration, Instant},
};

use futures::{
    channel::{mpsc, oneshot},
    sink::SinkMapErr,
    FutureExt, SinkExt, StreamExt,
};
use tower::load_shed::error::Overloaded;
use tracing::Span;

use zebra_chain::serialization::SerializationError;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    constants::{MAX_OVERLOAD_DROP_PROBABILITY, MIN_OVERLOAD_DROP_PROBABILITY, REQUEST_TIMEOUT},
    peer::{
        connection::{overload_drop_connection_probability, Connection, State},
        ClientRequest, ErrorSlot,
    },
    protocol::external::Message,
    types::Nonce,
    PeerError, Request, Response,
};

/// Test that the connection run loop works as a future
#[tokio::test]
async fn connection_run_loop_ok() {
    let _init_guard = zebra_test::init();

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
    assert!(error.is_none(), "unexpected error: {error:?}");

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
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (connection, client_tx, mut inbound_service, mut peer_outbound_messages, shared_error_slot) =
        new_test_connection();

    // Spawn the connection run loop
    let mut connection_join_handle = tokio::spawn(connection.run(peer_rx));

    let error = shared_error_slot.try_get_error();
    assert!(error.is_none(), "unexpected error: {error:?}");

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
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop works as a spawned task with messages in and out
#[tokio::test]
async fn connection_run_loop_message_ok() {
    let _init_guard = zebra_test::init();

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
    assert!(error.is_none(), "unexpected error: {error:?}");

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
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when dropped
#[tokio::test]
async fn connection_run_loop_future_drop() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
        "expected run loop termination, but run loop continued: {connection_result:?}",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop fails correctly when sending a message to a peer times out,
/// and we are expecting a response message from the peer.
#[tokio::test]
async fn connection_run_loop_send_timeout_expect_response() {
    let _init_guard = zebra_test::init();

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
        "expected run loop termination, but run loop continued: {connection_result:?}",
    );

    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Test that the connection run loop continues but returns an error to the client,
/// when a peer accepts a message, but does not send an expected response.
#[tokio::test]
async fn connection_run_loop_receive_timeout() {
    let _init_guard = zebra_test::init();

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
    assert!(error.is_none(), "unexpected error: {error:?}");

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
        "unexpected run loop termination: {connection_result:?}",
    );

    // We need to abort the connection, because it holds a lock on the outbound channel.
    connection_join_handle.abort();
    let outbound_message = peer_outbound_messages.next().await;
    assert_eq!(outbound_message, None);
}

/// Check basic properties of overload probabilities
#[test]
fn overload_probability_reduces_over_time() {
    let now = Instant::now();

    // Edge case: previous is in the future due to OS monotonic clock bugs
    let prev = now + Duration::from_secs(1);
    assert_eq!(
        overload_drop_connection_probability(now, Some(prev)),
        MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overload time is in the future (OS bugs?), it should have maximum drop probability",
    );

    // Overload/DoS case/edge case: rapidly repeated overloads
    let prev = now;
    assert_eq!(
        overload_drop_connection_probability(now, Some(prev)),
        MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overload times are the same, overloads should have maximum drop probability",
    );

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_micros(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability <= MAX_OVERLOAD_DROP_PROBABILITY,
        "if the overloads are very close together, drops can optionally decrease: {drop_probability} <= {MAX_OVERLOAD_DROP_PROBABILITY}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_millis(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload/DoS case: rapidly repeated overloads
    let prev = now - Duration::from_millis(10);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.001,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: frequent overloads
    let prev = now - Duration::from_millis(100);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability < 0.01,
        "if the overloads are very close together, drops can only decrease slightly: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: occasional but repeated overloads
    let prev = now - Duration::from_secs(1);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert!(
        MAX_OVERLOAD_DROP_PROBABILITY - drop_probability > 0.4,
        "if the overloads are distant, drops should decrease a lot: {drop_probability}",
    );
    let last_probability = drop_probability;

    // Overload case: occasional overloads
    let prev = now - Duration::from_secs(5);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert!(
        drop_probability < last_probability,
        "if the overloads decrease, drops should decrease: {drop_probability} < {last_probability}",
    );
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if overloads are far apart, drops should have minimum drop probability: {drop_probability}",
    );
    let _last_probability = drop_probability;

    // Base case: infrequent overloads
    let prev = now - Duration::from_secs(10);
    let drop_probability = overload_drop_connection_probability(now, Some(prev));
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if overloads are far apart, drops should have minimum drop probability: {drop_probability}",
    );

    // Base case: no previous overload
    let drop_probability = overload_drop_connection_probability(now, None);
    assert_eq!(
        drop_probability, MIN_OVERLOAD_DROP_PROBABILITY,
        "if there is no previous overload time, overloads should have minimum drop probability: {drop_probability}",
    );
}

/// Test that connections are randomly terminated in response to `Overloaded` errors.
///
/// TODO: do a similar test on the real service stack created in the `start` command.
#[tokio::test(flavor = "multi_thread")]
async fn connection_is_randomly_disconnected_on_overload() {
    let _init_guard = zebra_test::init();

    // The number of times we repeat the test
    const TEST_RUNS: usize = 220;
    // The expected number of tests before a test failure due to random chance.
    // Based on 10 tests per PR, 100 PR pushes per week, 50 weeks per year.
    const TESTS_BEFORE_FAILURE: f32 = 50_000.0;

    let test_runs = TEST_RUNS.try_into().expect("constant fits in i32");
    // The probability of random test failure is:
    // MIN_OVERLOAD_DROP_PROBABILITY^TEST_RUNS + MAX_OVERLOAD_DROP_PROBABILITY^TEST_RUNS
    assert!(
        1.0 / MIN_OVERLOAD_DROP_PROBABILITY.powi(test_runs) > TESTS_BEFORE_FAILURE,
        "not enough test runs: failures must be frequent enough to happen in almost all tests"
    );
    assert!(
        1.0 / MAX_OVERLOAD_DROP_PROBABILITY.powi(test_runs) > TESTS_BEFORE_FAILURE,
        "not enough test runs: successes must be frequent enough to happen in almost all tests"
    );

    let mut connection_continues = 0;
    let mut connection_closes = 0;

    for _ in 0..TEST_RUNS {
        // The real stream and sink are from a split TCP connection,
        // but that doesn't change how the state machine behaves.
        let (mut peer_tx, peer_rx) = mpsc::channel(1);

        let (
            connection,
            _client_tx,
            mut inbound_service,
            mut peer_outbound_messages,
            shared_error_slot,
        ) = new_test_connection();

        // The connection hasn't run so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before starting the connection event loop: {error:?}",
        );

        // Start the connection run loop future in a spawned task
        let connection_handle = tokio::spawn(connection.run(peer_rx));
        tokio::time::sleep(Duration::from_millis(1)).await;

        // The connection hasn't received any messages, so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before sending messages to the connection event loop: {error:?}",
        );

        // Simulate an overloaded connection error in response to an inbound request.
        let inbound_req = Message::GetAddr;
        peer_tx
            .send(Ok(inbound_req))
            .await
            .expect("send to channel always succeeds");
        tokio::time::sleep(Duration::from_millis(1)).await;

        // The connection hasn't got a response, so it must not have errors
        let error = shared_error_slot.try_get_error();
        assert!(
            error.is_none(),
            "unexpected error before sending responses to the connection event loop: {error:?}",
        );

        inbound_service
            .expect_request(Request::Peers)
            .await
            .respond_error(Overloaded::new().into());
        tokio::time::sleep(Duration::from_millis(1)).await;

        let outbound_result = peer_outbound_messages.try_next();
        assert!(
            !matches!(outbound_result, Ok(Some(_))),
            "unexpected outbound message after Overloaded error:\n\
             {outbound_result:?}\n\
             note: TryRecvErr means there are no messages, Ok(None) means the channel is closed"
        );

        let error = shared_error_slot.try_get_error();
        if error.is_some() {
            connection_closes += 1;
        } else {
            connection_continues += 1;
        }

        // We need to terminate the spawned task
        connection_handle.abort();
    }

    assert!(
        connection_closes > 0,
        "some overloaded connections must be closed at random"
    );
    assert!(
        connection_continues > 0,
        "some overloaded errors must be ignored at random"
    );
}

#[tokio::test]
async fn connection_ping_pong_round_trip() {
    let _init_guard = zebra_test::init();

    // The real stream and sink are from a split TCP connection,
    // but that doesn't change how the state machine behaves.
    let (peer_tx, peer_rx) = mpsc::channel(1);

    let (
        connection,
        mut client_tx,
        _inbound_service,
        mut peer_outbound_messages,
        _shared_error_slot,
    ) = new_test_connection();

    let connection = tokio::spawn(connection.run(peer_rx));

    // === Client sends Ping request ===
    let (response_tx, response_rx) = oneshot::channel();
    let nonce = Nonce::default();

    client_tx
        .send(ClientRequest {
            request: Request::Ping(nonce),
            tx: response_tx,
            inv_collector: None,
            transient_addr: None,
            span: Span::none(),
        })
        .await
        .expect("send to connection should succeed");

    // === Peer receives Ping message ===
    let outbound_msg = peer_outbound_messages
        .next()
        .await
        .expect("expected outbound Ping message");

    let ping_nonce = match outbound_msg {
        Message::Ping(nonce) => nonce,
        msg => panic!("expected Ping message, but got: {msg:?}",),
    };

    assert_eq!(
        nonce, ping_nonce,
        "Ping nonce in request must match message sent to peer"
    );

    // === Peer sends matching Pong ===
    let pong_rtt = Duration::from_millis(42);
    tokio::time::sleep(pong_rtt).await;

    peer_tx
        .clone()
        .send(Ok(Message::Pong(ping_nonce)))
        .await
        .expect("sending Pong to connection should succeed");

    // === Client receives Pong response and verifies RTT ===
    match response_rx.await.expect("response channel must succeed") {
        Ok(Response::Pong(rtt)) => {
            assert!(
                rtt >= pong_rtt,
                "measured RTT {rtt:?} must be >= simulated RTT {pong_rtt:?}"
            );
        }
        Ok(resp) => panic!("unexpected response: {resp:?}"),
        Err(err) => panic!("unexpected error: {err:?}"),
    }

    drop(peer_tx);
    let _ = connection.await;
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
