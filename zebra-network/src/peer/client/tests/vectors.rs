//! Fixed peer [`Client`] test vectors.

use tower::buffer::Buffer;

use zebra_test::service_extensions::IsReady;

use crate::{peer::ClientTestHarness, PeerError};

#[tokio::test]
async fn client_service_init_ok() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());

    // Do the readiness check last, so we don't have to deal with any capacity limits.
    assert!(client.is_ready().await);
}

#[tokio::test]
async fn client_service_ready_ok() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    // Keep the client alive until we've finished the test.
    //
    // Correctness: This won't cause capacity bugs, because:
    // - we only call `is_ready` (`poll_ready`) once, and:
    //     - the client's Buffer has a capacity of 1,
    //     - the ClientTestHarness request sender capacity is 1, and
    //     - none of the other channels have capacity limits.
    //
    // If any of these details change, the test could hang or panic.
    let client = Buffer::new(client, 1);
    let _client_guard = client.clone();

    assert!(client.is_ready().await);
    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

#[tokio::test]
async fn client_service_ready_heartbeat_exit() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::HeartbeatTaskExited);
    harness.drop_heartbeat_shutdown_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_ready_request_drop() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::ConnectionDropped);
    harness.drop_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
}

#[tokio::test]
async fn client_service_ready_request_close() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

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

    let (client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::Overloaded);

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_ready_multiple_errors() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::DuplicateHandshake);
    harness.drop_heartbeat_shutdown_receiver();
    harness.close_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_drop_cleanup() {
    zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    std::mem::drop(client);

    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}
