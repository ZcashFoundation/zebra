//! Fixed peer [`Client`] test vectors.

use std::iter;

use futures::poll;
use tokio::sync::broadcast;
use tower::ServiceExt;

use zebra_chain::block;
use zebra_test::service_extensions::IsReady;

use crate::{
    peer::{client::MissingInventoryCollector, ClientTestHarness},
    protocol::external::InventoryHash,
    PeerError, Request, SharedPeerError,
};

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

    // If the readiness future gains a `Drop` impl, we want it to be called here.
    #[allow(unknown_lints)]
    #[allow(clippy::drop_non_drop)]
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

/// Force the connection background task to stop, and check if the `Client` properly handles it.
#[tokio::test]
async fn client_service_handles_exited_connection_task() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.stop_connection_task().await;

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Force the connection background task to panic, and check if the `Client` propagates it.
#[tokio::test]
#[should_panic]
async fn client_service_propagates_panic_from_connection_task() {
    zebra_test::init();

    let (mut client, _harness) = ClientTestHarness::build()
        .with_connection_task(async move {
            panic!("connection task failure");
        })
        .finish();

    // Allow the custom connection task to run.
    tokio::task::yield_now().await;

    let _ = poll!(client.ready());
}

/// Force the heartbeat background task to stop, and check if the `Client` properly handles it.
#[tokio::test]
async fn client_service_handles_exited_heartbeat_task() {
    zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.stop_heartbeat_task().await;

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Force the heartbeat background task to panic, and check if the `Client` propagates it.
#[tokio::test]
#[should_panic]
async fn client_service_propagates_panic_from_heartbeat_task() {
    zebra_test::init();

    let (mut client, _harness) = ClientTestHarness::build()
        .with_heartbeat_task(async move {
            panic!("heartbeat task failure");
        })
        .finish();

    // Allow the custom heartbeat task to run.
    tokio::task::yield_now().await;

    let _ = poll!(client.ready());
}

/// Make sure MissingInventoryCollector ignores NotFoundRegistry errors.
///
/// ## Correctness
///
/// If the MissingInventoryCollector registered these locally generated errors,
/// our missing inventory errors could get constantly refreshed locally,
/// and we would never ask the peer if it has received the inventory.
#[test]
fn missing_inv_collector_ignores_local_registry_errors() {
    zebra_test::init();

    let block_hash = block::Hash([0; 32]);
    let request = Request::BlocksByHash(iter::once(block_hash).collect());
    let response = Err(SharedPeerError::from(PeerError::NotFoundRegistry(vec![
        InventoryHash::from(block_hash),
    ])));

    let (inv_collector, mut inv_receiver) = broadcast::channel(1);
    let transient_addr = "0.0.0.0:0".parse().unwrap();

    // Keep the channel open, so we don't get a `Closed` error.
    let _inv_channel_guard = inv_collector.clone();

    let missing_inv =
        MissingInventoryCollector::new(&request, Some(inv_collector), Some(transient_addr))
            .expect("unexpected invalid collector: arguments should be valid");

    missing_inv.send(&response);

    let recv_result = inv_receiver.try_recv();
    assert_eq!(recv_result, Err(broadcast::error::TryRecvError::Empty));
}
