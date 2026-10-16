//! Request-channel readiness must not inherit the later wire-response timer.

use std::time::Duration;

use futures::{channel::mpsc, stream, FutureExt, StreamExt};
use tower::{discover::Change, Service, ServiceExt};
use zebra_chain::{block, parameters::Network};
use zebra_test::mock_service::PanicAssertion;

use crate::{
    constants::{CURRENT_NETWORK_PROTOCOL_VERSION, REQUEST_TIMEOUT},
    peer::{ClientTestHarness, LoadTrackedClient, MinimumPeerVersion},
    peer_set::set::tests::PeerSetBuilder,
    protocol::external::{types::PeerServices, InventoryHash, Message},
    BoxError, PeerSocketAddr, Request, Response,
};

use super::new_test_connection;

/// Each inbound request completes promptly, but inbound priority keeps the outbound channel full.
/// A ready fallback must become usable once the existing request bound has elapsed.
#[tokio::test(start_paused = true)]
async fn backpressured_peer_has_bounded_routing_preference() {
    let _init_guard = zebra_test::init();
    let (connection, client_tx, mut inbound, mut outbound, error_slot) =
        new_test_connection::<PanicAssertion>();
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let (mut busy_client, _busy_harness) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_services(PeerServices::NODE_NETWORK)
        .with_connection_task(connection.run(peer_rx))
        .finish();
    busy_client.server_tx = client_tx;
    busy_client.error_slot = error_slot.clone();

    let advertised_hash = block::Hash([1; 32]);
    peer_tx
        .try_send(Ok(Message::Inv(vec![InventoryHash::Block(
            advertised_hash,
        )])))
        .unwrap();
    let mut inbound_response = inbound
        .expect_request(Request::AdvertiseBlock(advertised_hash, None))
        .await;

    // channel(0) still has one slot per sender. Fill the real Client's slot; do not assume
    // that creating the channel alone makes the client unready.
    let mut queued = busy_client.ready().await.unwrap().call(Request::Peers);
    assert!(busy_client.ready().now_or_never().is_none());
    assert!(queued.as_mut().now_or_never().is_none());
    assert!(outbound.next().now_or_never().is_none());

    let (fallback, mut fallback_harness) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_services(PeerServices::empty())
        .finish();
    let busy_addr: PeerSocketAddr = "127.0.0.1:1".parse().unwrap();
    let fallback_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
    let peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> = vec![
        Ok(Change::Insert(busy_addr, busy_client.into())),
        Ok(Change::Insert(fallback_addr, fallback.into())),
    ];
    let (minimum_version, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(stream::iter(peers).chain(stream::pending()))
        .with_minimum_peer_version(minimum_version)
        .build();
    peer_set.ready().await.unwrap();

    // Preserve useful waiting before the bound: a historic block must not immediately use
    // the non-serving fallback merely because the serving peer is temporarily full.
    let block_request = Request::BlocksByHash([block::Hash([2; 32])].into());
    let mut waiting = peer_set.call(block_request.clone());
    assert!(waiting.as_mut().now_or_never().is_none());
    assert!(fallback_harness
        .try_to_receive_outbound_client_request()
        .is_empty());
    drop(waiting);

    // Queue the next inbound message BEFORE completing the current one. The real Connection
    // select then prioritizes that message over its already queued outbound request. No single
    // inbound call approaches REQUEST_TIMEOUT, and no wire response timer has started.
    for step in 0..4 {
        peer_tx
            .try_send(Ok(Message::Inv(vec![InventoryHash::Block(
                advertised_hash,
            )])))
            .unwrap();
        tokio::time::advance(REQUEST_TIMEOUT / 4).await;
        inbound_response.respond(Response::Nil);
        inbound_response = inbound
            .expect_request(Request::AdvertiseBlock(advertised_hash, None))
            .await;
        if step < 3 {
            assert!(error_slot.try_get_error().is_none());
            assert!(queued.as_mut().now_or_never().is_none());
            assert!(outbound.next().now_or_never().is_none());
            peer_set.ready().await.unwrap();
        }
    }

    tokio::time::advance(Duration::from_millis(1)).await;
    let response = peer_set.ready().await.unwrap().call(block_request.clone());
    let routed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(request) = fallback_harness
                .try_to_receive_outbound_client_request()
                .request()
            {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("a peer unready beyond REQUEST_TIMEOUT must stop suppressing the ready fallback");
    assert_eq!(routed.request, block_request);
    routed.tx.send(Ok(Response::Blocks(Vec::new()))).unwrap();
    assert_eq!(response.await.unwrap(), Response::Blocks(Vec::new()));
    inbound_response.respond(Response::Nil);
}

/// The response timer still starts at actual dispatch, not when a client request is queued.
#[tokio::test(start_paused = true)]
async fn backpressured_request_gets_response_budget_after_dispatch() {
    let _init_guard = zebra_test::init();
    let (connection, mut client_tx, mut inbound, mut outbound, error_slot) =
        new_test_connection::<PanicAssertion>();
    let (mut peer_tx, peer_rx) = mpsc::channel(1);
    let connection_task = tokio::spawn(connection.run(peer_rx));
    let advertised_hash = block::Hash([3; 32]);
    peer_tx
        .try_send(Ok(Message::Inv(vec![InventoryHash::Block(
            advertised_hash,
        )])))
        .unwrap();
    let inbound_response = inbound
        .expect_request(Request::AdvertiseBlock(advertised_hash, None))
        .await;
    let (tx, rx) = futures::channel::oneshot::channel();
    client_tx
        .try_send(crate::peer::ClientRequest {
            request: Request::Peers,
            tx,
            inv_collector: None,
            transient_addr: None,
            span: tracing::Span::current(),
        })
        .unwrap();
    tokio::time::advance(REQUEST_TIMEOUT / 2).await;
    assert!(outbound.next().now_or_never().is_none());
    inbound_response.respond(Response::Nil);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.next())
            .await
            .unwrap(),
        Some(Message::GetAddr)
    );
    tokio::task::yield_now().await;
    tokio::time::advance(REQUEST_TIMEOUT * 3 / 4).await;
    peer_tx.try_send(Ok(Message::Addr(Vec::new()))).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Response::Peers(Vec::new())
    );
    assert!(error_slot.try_get_error().is_none());
    drop(client_tx);
    tokio::time::timeout(Duration::from_secs(1), connection_task)
        .await
        .unwrap()
        .unwrap();
}
