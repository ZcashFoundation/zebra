//! Fixed peer [`Client`] test vectors.

use tower::ServiceExt;

use zebra_test::service_extensions::IsReady;

use crate::{peer::ClientTestHarness, PeerError};

/// Test that a newly initialized client functions correctly before it is polled.
#[tokio::test]
async fn client_service_ok_without_readiness_check() {
    zebra_test::init();

    let (_client, mut harness) = ClientTestHarness::build().finish();

    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that a newly initialized client functions correctly after it is polled.
#[tokio::test]
async fn client_service_ready_ok() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    assert!(client.is_ready().await);
    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that a client functions correctly if its readiness future is dropped.
#[tokio::test]
async fn client_service_ready_drop_ok() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    std::mem::drop(client.ready());

    assert!(client.is_ready().await);
    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that a client functions correctly if it is polled for readiness multiple times.
#[tokio::test]
async fn client_service_ready_multiple_ok() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    assert!(client.is_ready().await);
    assert!(client.is_ready().await);

    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that clients propagate errors from their heartbeat tasks.
#[tokio::test]
async fn client_service_ready_heartbeat_exit() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::HeartbeatTaskExited);
    harness.drop_heartbeat_shutdown_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Test that clients propagate errors from their connection tasks.
#[tokio::test]
async fn client_service_ready_request_drop() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::ConnectionDropped);
    harness.drop_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
}

/// Test that clients error when their connection task closes the request channel.
#[tokio::test]
async fn client_service_ready_request_close() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::ConnectionClosed);
    harness.close_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_ready_error_in_slot() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::Overloaded);

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Test that clients error when multiple error conditions occur at the same time.
#[tokio::test]
async fn client_service_ready_multiple_errors() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::DuplicateHandshake);
    harness.drop_heartbeat_shutdown_receiver();
    harness.close_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Test that clients register an error and cleanup channels correctly when the client is dropped.
#[tokio::test]
async fn client_service_drop_cleanup() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    std::mem::drop(client);

    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}
