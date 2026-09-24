//! Fixed peer [`Client`] test vectors.

use std::iter;

use futures::{poll, FutureExt, StreamExt};
use tokio::sync::broadcast;
use tower::{Service, ServiceExt};

use zebra_chain::block;
use zebra_test::service_extensions::IsReady;

use crate::{
    peer::{
        client::MissingInventoryCollector, ClientRequestReceiver, ClientTestHarness, ConnectedAddr,
        LoadTrackedClient,
    },
    peer_set::InventoryChange,
    protocol::external::InventoryHash,
    PeerError, Request, Response, SharedPeerError,
};

/// Test that a newly initialized client functions correctly before it is polled.
#[tokio::test]
async fn client_service_ok_without_readiness_check() {
    let _init_guard = zebra_test::init();

    let (_client, mut harness) = ClientTestHarness::build().finish();

    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that a newly initialized client functions correctly after it is polled.
#[tokio::test]
async fn client_service_ready_ok() {
    let _init_guard = zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    assert!(client.is_ready().await);
    assert!(harness.current_error().is_none());
    assert!(harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_empty());
}

/// Test that a client functions correctly if its readiness future is dropped.
#[tokio::test]
async fn client_service_ready_drop_ok() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

    let (mut client, mut harness) = ClientTestHarness::build().finish();

    harness.set_error(PeerError::HeartbeatTaskExited("some error".to_string()));
    harness.drop_heartbeat_shutdown_receiver();

    assert!(client.is_failed().await);
    assert!(harness.current_error().is_some());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Test that clients propagate errors from their connection tasks.
#[tokio::test]
async fn client_service_ready_request_drop() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

    let (client, mut harness) = ClientTestHarness::build().finish();

    std::mem::drop(client);

    assert!(harness.current_error().is_some());
    assert!(!harness.wants_connection_heartbeats());
    assert!(harness.try_to_receive_outbound_client_request().is_closed());
}

/// Force the connection background task to stop, and check if the `Client` properly handles it.
#[tokio::test]
async fn client_service_handles_exited_connection_task() {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

    let (mut client, _harness) = ClientTestHarness::build()
        .with_heartbeat_task(async move {
            panic!("heartbeat task failure");
        })
        .finish();

    // Allow the custom heartbeat task to run.
    tokio::task::yield_now().await;

    let _ = poll!(client.ready());
}

/// Only block request results update the block-serving health signal.
#[tokio::test]
async fn client_block_request_failure_is_reset_only_by_block_response() {
    let _init_guard = zebra_test::init();
    let (client, mut harness) = ClientTestHarness::build().finish();
    let mut client = LoadTrackedClient::from(client);
    let block_request = Request::BlocksByHash([block::Hash([0; 32])].into());
    assert!(!client.last_block_request_failed());

    let response = client.ready().await.unwrap().call(block_request.clone());
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
        .unwrap();
    assert!(client.last_block_request_failed());
    assert!(response.await.is_err());
    assert!(client.last_block_request_failed());

    for (request, reply) in [
        (
            Request::Ping(Default::default()),
            Response::Pong(Default::default()),
        ),
        (
            Request::TransactionsById(Default::default()),
            Response::Transactions(Vec::new()),
        ),
    ] {
        let response = client.ready().await.unwrap().call(request);
        harness
            .try_to_receive_outbound_client_request()
            .request()
            .unwrap()
            .respond(Ok(reply))
            .unwrap();
        response.await.unwrap();
        assert!(client.last_block_request_failed());
    }

    let response = client.ready().await.unwrap().call(block_request);
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Ok(Response::Blocks(Vec::new())))
        .unwrap();
    assert!(!client.last_block_request_failed());
    response.await.unwrap();
    assert!(!client.last_block_request_failed());

    for request in [
        Request::Ping(Default::default()),
        Request::TransactionsById(Default::default()),
    ] {
        let response = client.ready().await.unwrap().call(request);
        harness
            .try_to_receive_outbound_client_request()
            .request()
            .unwrap()
            .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
            .unwrap();
        assert!(response.await.is_err());
        assert!(!client.last_block_request_failed());
    }
}

/// Queued and in-progress cancellations update health when the connection acknowledges them.
#[tokio::test]
async fn client_cancelled_block_requests_fail_when_connection_acknowledges() {
    let _init_guard = zebra_test::init();
    let addr = "127.0.0.1:8233".parse().unwrap();
    let (client, mut harness) = ClientTestHarness::build()
        .with_connected_addr(ConnectedAddr::new_inbound_direct(addr))
        .finish();
    let mut client = LoadTrackedClient::from(client);
    let mut receiver = ClientRequestReceiver::from(harness.client_request_receiver.take().unwrap());
    let block_request = Request::BlocksByHash([block::Hash([0; 32])].into());

    for receive_before_cancel in [false, true] {
        let mut response = client
            .ready()
            .await
            .unwrap()
            .call(block_request.clone())
            .boxed();
        let request = if receive_before_cancel {
            assert!(poll!(response.as_mut()).is_pending());
            Some(receiver.next().await.unwrap())
        } else {
            None
        };
        drop(response);
        assert!(!client.last_block_request_failed());

        let mut request = match request {
            Some(request) => request,
            None => receiver.next().await.unwrap(),
        };
        assert!(request.tx.cancellation().now_or_never().is_some());
        drop(request);
        assert!(client.last_block_request_failed());
        assert!(harness.try_to_receive_inventory_change().is_none());

        let response = client.ready().await.unwrap().call(block_request.clone());
        receiver
            .next()
            .await
            .unwrap()
            .tx
            .send(Ok(Response::Blocks(Vec::new())))
            .unwrap();
        assert!(!client.last_block_request_failed());
        response.await.unwrap();
    }
}

/// Observing responses in reverse order cannot overwrite the latest connection outcome.
#[tokio::test]
async fn client_block_health_follows_connection_completion_order() {
    let _init_guard = zebra_test::init();
    let (client, mut harness) = ClientTestHarness::build().finish();
    let mut client = LoadTrackedClient::from(client);
    let block_request = Request::BlocksByHash([block::Hash([0; 32])].into());

    let old_response = client.ready().await.unwrap().call(block_request.clone());
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
        .unwrap();
    assert!(client.last_block_request_failed());

    let new_response = client.ready().await.unwrap().call(block_request);
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Ok(Response::Blocks(Vec::new())))
        .unwrap();
    assert!(!client.last_block_request_failed());
    new_response.await.unwrap();
    assert!(old_response.await.is_err());
    assert!(!client.last_block_request_failed());
}

/// Dropping an already completed response is not a new cancellation.
#[tokio::test]
async fn client_dropping_completed_block_responses_preserves_latest_health() {
    let _init_guard = zebra_test::init();
    let (client, mut harness) = ClientTestHarness::build().finish();
    let mut client = LoadTrackedClient::from(client);
    let block_request = Request::BlocksByHash([block::Hash([0; 32])].into());

    let old_response = client.ready().await.unwrap().call(block_request.clone());
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
        .unwrap();

    let new_response = client.ready().await.unwrap().call(block_request);
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Ok(Response::Blocks(Vec::new())))
        .unwrap();
    assert!(!client.last_block_request_failed());
    drop(old_response);
    assert!(!client.last_block_request_failed());
    drop(new_response);
    assert!(!client.last_block_request_failed());
}

/// Explicit missing inventory restores responsiveness; timeouts never mark inventory missing.
#[tokio::test]
async fn client_block_notfound_recovers_health_without_timeout_inventory() {
    let _init_guard = zebra_test::init();
    let addr = "127.0.0.1:8233".parse().unwrap();
    let (client, mut harness) = ClientTestHarness::build()
        .with_connected_addr(ConnectedAddr::new_inbound_direct(addr))
        .finish();
    let mut client = LoadTrackedClient::from(client);
    let block_hash = block::Hash([0; 32]);
    let block_request = Request::BlocksByHash([block_hash].into());

    let timed_out = client.ready().await.unwrap().call(block_request.clone());
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
        .unwrap();
    assert!(client.last_block_request_failed());
    assert!(harness.try_to_receive_inventory_change().is_none());

    let not_found = client.ready().await.unwrap().call(block_request);
    harness
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap()
        .respond(Err(
            PeerError::NotFoundResponse(vec![block_hash.into()]).into()
        ))
        .unwrap();
    assert!(!client.last_block_request_failed());
    assert_eq!(
        harness.try_to_receive_inventory_change(),
        Some(InventoryChange::new_missing(block_hash.into(), addr)),
    );
    assert!(not_found.await.is_err());
    assert!(timed_out.await.is_err());
    assert!(!client.last_block_request_failed());
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
    let _init_guard = zebra_test::init();

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
