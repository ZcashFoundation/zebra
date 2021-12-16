//! Fixed peer [`Client`] test vectors.

use zebra_test::service_extensions::IsReady;

use crate::{peer::ClientTestHarness, protocol::external::types::Version, PeerError};

#[tokio::test]
async fn client_service_ready_ok() {
    zebra_test::init();

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

    assert!(client.is_ready().await);
    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

#[tokio::test]
async fn client_service_ready_heartbeat_exit() {
    zebra_test::init();

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

    harness.set_error(PeerError::HeartbeatTaskExited);
    harness.drop_heartbeat_shutdown_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_ready_request_drop() {
    zebra_test::init();

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

    harness.set_error(PeerError::ConnectionDropped);
    harness.drop_outbound_client_request_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
}

#[tokio::test]
async fn client_service_ready_request_close() {
    zebra_test::init();

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

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

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

    harness.set_error(PeerError::Overloaded);

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

#[tokio::test]
async fn client_service_ready_multiple_errors() {
    zebra_test::init();

    let (mut harness, mut client) = ClientTestHarness::new(Version(0));

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

    let (mut harness, client) = ClientTestHarness::new(Version(0));

    std::mem::drop(client);

    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}
