//! Fixed test vectors for the peer set.

use std::{
    cmp::max,
    iter,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use futures::{channel::mpsc, stream, Future, FutureExt as _, SinkExt, Stream, StreamExt};
use tokio::time::timeout;
use tower::{
    discover::{Change, Discover},
    Service, ServiceExt,
};

use zebra_chain::{
    block,
    chain_tip::{
        mock::{MockChainTip, MockChainTipSender},
        ChainTip,
    },
    parameters::{Network, NetworkUpgrade},
    serialization::{SerializationError, ZcashDeserializeInto},
    transaction::{self, UnminedTxId},
};

use crate::{
    constants::{
        CURRENT_NETWORK_PROTOCOL_VERSION, DEFAULT_MAX_CONNS_PER_IP,
        FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, INVENTORY_BUSY_PEER_WAIT_TIMEOUT, PEERSET_BUFFER_SIZE,
        REQUEST_TIMEOUT,
    },
    peer::{
        Client, ClientRequest, ClientTestHarness, ConnectedAddr, Connection, LoadTrackedClient,
        MinimumPeerVersion,
    },
    peer_set::{
        inventory_registry::InventoryStatus, stall_tracker::FIND_RESPONSE_STALL_THRESHOLD,
        ActiveConnectionCounter, InventoryChange, PeerSet,
    },
    protocol::external::{
        types::{PeerServices, Version},
        InventoryHash, Message,
    },
    BoxError, PeerError, PeerSocketAddr, Request, Response, SharedPeerError,
};
use tokio::sync::watch;

use super::{mock_peer_discovery_with_start_height, PeerSetBuilder, PeerSetGuard, PeerVersions};

#[path = "find_capacity.rs"]
mod find_capacity;

#[path = "failure_reviews.rs"]
mod failure_reviews;

#[path = "eligibility_reviews.rs"]
mod eligibility_reviews;

#[path = "early_relay.rs"]
mod early_relay;

#[test]
fn peer_set_ready_single_connection() {
    // We are going to use just one peer version in this test
    let peer_versions = PeerVersions {
        peer_versions: vec![Version::min_specified_for_upgrade(
            &Network::Mainnet,
            NetworkUpgrade::Nu6_2,
        )],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Get peers and client handles of them
    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // We will just use the first peer handle
    let mut client_handle = handles
        .into_iter()
        .next()
        .expect("we always have at least one client");

    // Client did not received anything yet
    assert!(client_handle
        .try_to_receive_outbound_client_request()
        .is_empty());

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Get a ready future
        let peer_ready_future = peer_set.ready();

        // If the readiness future gains a `Drop` impl, we want it to be called here.
        #[allow(unknown_lints)]
        #[allow(clippy::drop_non_drop)]
        std::mem::drop(peer_ready_future);

        // Peer set will remain ready for requests
        let peer_ready1 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Make sure the client did not received anything yet
        assert!(client_handle
            .try_to_receive_outbound_client_request()
            .is_empty());

        // Make a call to the peer set that returns a future
        let fut = peer_ready1.call(Request::Peers);

        // Client received the request
        assert!(matches!(
            client_handle
                .try_to_receive_outbound_client_request()
                .request(),
            Some(ClientRequest {
                request: Request::Peers,
                ..
            })
        ));

        // Drop the future
        std::mem::drop(fut);

        // Peer set will remain ready for requests
        let peer_ready2 = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Get a new future calling a different request than before
        let _fut = peer_ready2.call(Request::MempoolTransactionIds);

        // Client received the request
        assert!(matches!(
            client_handle
                .try_to_receive_outbound_client_request()
                .request(),
            Some(ClientRequest {
                request: Request::MempoolTransactionIds,
                ..
            })
        ));
    });
}

#[test]
fn peer_set_ready_multiple_connections() {
    // Use three peers with the same version
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get peers and client handles of them
    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), 3);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(3, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(peer_ready.ready_services.len(), 3);

        // Stop some peer connections but not all
        handles[0].stop_connection_task().await;
        handles[1].stop_connection_task().await;

        // We can still make the peer set ready
        peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Stop the connection of the last peer
        handles[2].stop_connection_task().await;

        // Peer set hangs when no more connections are present
        let peer_ready = peer_set.ready();
        assert!(timeout(Duration::from_secs(10), peer_ready).await.is_err());
    });
}

#[test]
fn peer_set_rejects_connections_past_per_ip_limit() {
    const NUM_PEER_VERSIONS: usize = crate::constants::DEFAULT_MAX_CONNS_PER_IP + 1;

    // Use three peers with the same version
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: [peer_version; NUM_PEER_VERSIONS].into_iter().collect(),
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get peers and client handles of them
    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), NUM_PEER_VERSIONS);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(
            peer_ready.ready_services.len(),
            crate::constants::DEFAULT_MAX_CONNS_PER_IP
        );
    });
}

/// Check that a peer set with an empty inventory registry routes requests to a random ready peer.
#[test]
fn peer_set_route_inv_empty_registry() {
    let test_hash = block::Hash([0; 32]);

    // Use two peers with the same version
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get peers and client handles of them
    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), 2);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(peer_ready.ready_services.len(), 2);

        // Send an inventory-based request
        let sent_request = Request::BlocksByHash(iter::once(test_hash).collect());
        let _fut = peer_ready.call(sent_request.clone());

        // Check that one of the clients received the request
        let mut received_count = 0;
        for mut handle in handles {
            if let Some(ClientRequest { request, .. }) =
                handle.try_to_receive_outbound_client_request().request()
            {
                assert_eq!(sent_request, request);
                received_count += 1;
            }
        }

        assert_eq!(received_count, 1);
    });
}

/// A busy peer receives the advert before its request channel has any capacity.
#[tokio::test]
async fn block_advert_to_all_reaches_unready_peer() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version; 2],
    };
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
        .with_discover(discovered_peers)
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
        .build();

    timeout(Duration::from_secs(1), async {
        peer_set.ready().await.unwrap();
        let busy_addr: PeerSocketAddr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let mut busy_service = peer_set.take_ready_service(&busy_addr).unwrap();

        // The mock has one buffered slot plus its sender's reserved slot.
        let _first_request = busy_service.call(Request::Peers);
        busy_service.ready().await.unwrap();
        let _second_request = busy_service.call(Request::Peers);
        assert!(busy_service.ready().now_or_never().is_none());
        peer_set.push_unready(busy_addr, busy_service);

        let hash = block::Hash([9; 32]);
        assert!(peer_set.cancel_handles.contains_key(&busy_addr));
        assert!(!peer_set.ready_services.contains_key(&busy_addr));
        let mut advertiser = peer_set.block_advertiser();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        for handle in &mut handles {
            assert_eq!(handle.receive_block_advert(), hash);
        }
        assert_eq!(advert.await.unwrap(), Response::Nil);
    })
    .await
    .expect("the ready peer permits dispatch while the busy peer remains unready");
}

/// The production Buffer must not strand adverts when every ordinary request channel is full.
#[tokio::test(start_paused = true)]
async fn block_advert_bypasses_buffer_with_all_peers_busy() {
    let _init_guard = zebra_test::init();
    let peer_versions = PeerVersions {
        peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION; 2],
    };
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discovered_peers)
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
        .build();
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let mut pending_requests = Vec::new();
    for port in 1..=2 {
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port).into();
        let mut service = peer_set.take_ready_service(&addr).unwrap();
        pending_requests.push(service.call(Request::Peers));
        service.ready().await.unwrap();
        pending_requests.push(service.call(Request::Peers));
        assert!(service.ready().now_or_never().is_none());
        peer_set.push_unready(addr, service);
    }
    assert!(
        peer_set.ready().now_or_never().is_none(),
        "ordinary requests must remain backpressured when every peer is busy"
    );

    let mut advertiser = peer_set.block_advertiser();
    let (mut network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    let mut ordinary = Box::pin(network.ready().await.unwrap().call(Request::Peers));
    assert!(ordinary.as_mut().now_or_never().is_none());
    tokio::task::yield_now().await;
    let hash = block::Hash([13; 32]);
    let advert = advertiser
        .ready()
        .await
        .unwrap()
        .call(Request::AdvertiseBlockToAll(hash));
    for handle in &mut handles {
        let queued = timeout(Duration::from_secs(1), handle.block_advert_receiver.recv())
            .await
            .expect("adverts bypass the blocked ordinary buffer")
            .unwrap();
        assert_eq!(queued.hash, hash);
        queued.result.send(Ok(())).unwrap();
    }
    assert_eq!(advert.await.unwrap(), Response::Nil);
    assert!(ordinary.as_mut().now_or_never().is_none());
    drop(pending_requests);
}

/// Discovery must reach block fanout without another ordinary buffered request.
#[tokio::test(start_paused = true)]
async fn block_advert_to_all_discovers_peer_while_buffer_idle() {
    let _init_guard = zebra_test::init();
    advert_discovers_peer_while_buffer_idle(true).await;
}

/// Newly discovered sidecars must join sampled fanout even when ordinary requests are idle.
#[tokio::test(start_paused = true)]
async fn sampled_block_advert_discovers_sidecar_while_buffer_idle() {
    let _init_guard = zebra_test::init();
    advert_discovers_peer_while_buffer_idle(false).await;
}

async fn advert_discovers_peer_while_buffer_idle(all_peers: bool) {
    timeout(Duration::from_secs(2), async {
        let (old_client, _old_harness, old_connection, mut old_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (new_client, _new_harness, new_connection, mut new_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (mut discovery, discovered) = mpsc::channel::<
            Result<Change<PeerSocketAddr, crate::peer::LoadTrackedClient>, BoxError>,
        >(1);
        let old_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let new_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 2).into();
        discovery
            .try_send(Ok(Change::Insert(old_addr, old_client.into())))
            .unwrap();
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .with_block_gossip_peer_ips(if all_peers {
                Vec::new()
            } else {
                vec![Ipv4Addr::LOCALHOST.into()]
            })
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let mut advertiser = peer_set.block_advertiser();
        let (mut network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let old_task = tokio::spawn(old_connection);
        let new_task = tokio::spawn(new_connection);

        // Dispatch one ordinary request through the real Buffer. Its worker becomes idle after
        // forwarding the response future; retaining that future keeps the connection request alive.
        let _ordinary = network.ready().await.unwrap().call(Request::Peers);
        assert_eq!(old_messages.next().await, Some(Message::GetAddr));
        tokio::task::yield_now().await;

        // No ordinary request or direct PeerSet readiness poll occurs after this discovery.
        discovery
            .try_send(Ok(Change::Insert(new_addr, new_client.into())))
            .unwrap();
        tokio::task::yield_now().await;
        let hash = block::Hash([19; 32]);
        let request = if all_peers {
            Request::AdvertiseBlockToAll(hash)
        } else {
            Request::AdvertiseBlock(hash, None)
        };
        let advert = advertiser.ready().await.unwrap().call(request);
        let (delivery, old_write, new_write) = tokio::join!(
            advert,
            old_messages.next(),
            timeout(Duration::from_secs(1), new_messages.next()),
        );
        old_task.abort();
        new_task.abort();
        assert_eq!(
            old_write,
            Some(Message::Inv(vec![InventoryHash::Block(hash)]))
        );
        assert_eq!(delivery.unwrap(), Response::Nil);
        assert_eq!(
            new_write.expect("idle ordinary traffic must not strand a newly discovered peer"),
            Some(Message::Inv(vec![InventoryHash::Block(hash)])),
        );
    })
    .await
    .expect("idle-buffer discovery and advert delivery must remain bounded");
}

/// A barrier submitted after discovery returned Pending must wait for a fresh reconciliation pass.
#[tokio::test(start_paused = true)]
async fn block_advert_does_not_ack_a_barrier_arriving_during_discovery() {
    use std::{cell::RefCell, rc::Rc, task::Poll};
    use zebra_chain::chain_tip::mock::MockChainTip;

    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        let (old_client, _old_harness, old_connection, mut old_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (new_client, _new_harness, new_connection, mut new_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (mut discovery, mut discovered) = mpsc::channel::<
            Result<Change<PeerSocketAddr, crate::peer::LoadTrackedClient>, BoxError>,
        >(1);
        let old_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let new_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 2).into();
        discovery
            .try_send(Ok(Change::Insert(old_addr, old_client.into())))
            .unwrap();
        let advertiser = Rc::new(RefCell::new(None::<crate::BlockAdvertiser<MockChainTip>>));
        let during_discovery = advertiser.clone();
        let mut late_peer = Some(new_client);
        let discovered = stream::poll_fn(move |cx| {
            let polled = discovered.poll_next_unpin(cx);
            if polled.is_pending() {
                if let Some(client) = late_peer.take() {
                    // The current pass has already sampled an empty discovery stream. Queue a
                    // peer, then submit a real advertiser readiness barrier before returning that
                    // Pending result. Neither is part of the snapshot this pass reconciled.
                    discovery
                        .try_send(Ok(Change::Insert(new_addr, client.into())))
                        .unwrap();
                    assert!(during_discovery
                        .borrow_mut()
                        .as_mut()
                        .unwrap()
                        .poll_ready(cx)
                        .is_pending());
                }
            }
            polled
        });
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (mut peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        *advertiser.borrow_mut() = Some(peer_set.block_advertiser());
        let old_task = tokio::spawn(old_connection);
        let new_task = tokio::spawn(new_connection);

        // Drive the production readiness methods until the advertiser is acknowledged. The
        // buggy implementation acknowledges in the first pass; the fix must request another pass
        // and admit the intervening discovery before this future completes.
        futures::future::poll_fn(|cx| {
            if let Poll::Ready(Err(error)) = peer_set.poll_ready(cx) {
                return Poll::Ready(Err(error));
            }
            advertiser.borrow_mut().as_mut().unwrap().poll_ready(cx)
        })
        .await
        .unwrap();
        let hash = block::Hash([22; 32]);
        let advert = advertiser
            .borrow_mut()
            .as_mut()
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        let (delivery, old_write, new_write) = tokio::join!(
            advert,
            old_messages.next(),
            timeout(Duration::from_secs(1), new_messages.next()),
        );
        old_task.abort();
        new_task.abort();
        assert_eq!(
            old_write,
            Some(Message::Inv(vec![InventoryHash::Block(hash)]))
        );
        assert_eq!(delivery.unwrap(), Response::Nil);
        assert_eq!(
            new_write
                .expect("a maintenance barrier must include discovery queued before that barrier"),
            Some(Message::Inv(vec![InventoryHash::Block(hash)])),
        );
    })
    .await
    .expect("interleaved discovery and maintenance acknowledgment must remain bounded");
}

/// Captured waiters survive a discovery burst spanning multiple bounded maintenance passes.
#[tokio::test(start_paused = true)]
async fn block_advert_barriers_survive_partial_discovery_drains() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        let config = crate::Config::default();
        let batch = config.peerset_total_connection_limit();
        let (old_client, _old_harness, old_connection, mut old_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (new_client, _new_harness, new_connection, mut new_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let old_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let new_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 2).into();
        let absent_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 3).into();
        let discovered = stream::iter([Ok::<_, BoxError>(Change::Insert(
            old_addr,
            old_client.into(),
        ))])
        .chain(stream::iter(
            (0..batch).map(move |_| Ok(Change::Remove(absent_addr))),
        ))
        .chain(stream::iter([Ok(Change::Insert(
            new_addr,
            new_client.into(),
        ))]))
        .chain(stream::pending());
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, _guard) = PeerSetBuilder::new()
            .with_config(config)
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let advertiser = peer_set.block_advertiser();
        let mut waiting: Vec<_> = (0..PEERSET_BUFFER_SIZE)
            .map(|_| advertiser.clone())
            .collect();
        for advertiser in &mut waiting {
            assert!(advertiser.ready().now_or_never().is_none());
        }
        // Every barrier precedes reconciliation; the final peer lies beyond the first batch.
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let old_task = tokio::spawn(old_connection);
        let new_task = tokio::spawn(new_connection);
        let hash = block::Hash([23; 32]);
        for advertiser in &mut waiting {
            let advert = advertiser
                .ready()
                .await
                .unwrap()
                .call(Request::AdvertiseBlockToAll(hash));
            let (delivery, old_write, new_write) =
                tokio::join!(advert, old_messages.next(), new_messages.next());
            assert_eq!(delivery.unwrap(), Response::Nil);
            assert_eq!(
                old_write,
                Some(Message::Inv(vec![InventoryHash::Block(hash)]))
            );
            assert_eq!(
                new_write,
                Some(Message::Inv(vec![InventoryHash::Block(hash)]))
            );
        }
        old_task.abort();
        new_task.abort();
    })
    .await
    .expect("captured maintenance waiters must complete after discovery drains");
}

/// Finishing a retained full batch must schedule waiters whose earlier wake was already consumed.
#[tokio::test(start_paused = true)]
async fn block_advert_late_barrier_is_rescheduled_after_retained_batch() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        let config = crate::Config::default();
        let batch = config.peerset_total_connection_limit();
        let (client, _harness, connection, mut messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let absent = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 2).into();
        let discovered = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
            .chain(stream::iter(
                (0..batch).map(move |_| Ok(Change::Remove(absent))),
            ))
            .chain(stream::pending());
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (mut peer_set, _guard) = PeerSetBuilder::new()
            .with_config(config)
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .build();
        let advertiser = peer_set.block_advertiser();
        let mut waiting: Vec<_> = (0..PEERSET_BUFFER_SIZE)
            .map(|_| advertiser.clone())
            .collect();
        for advertiser in &mut waiting {
            assert!(advertiser.ready().now_or_never().is_none());
        }
        peer_set
            .ready()
            .now_or_never()
            .expect("the first peer is ordinarily ready")
            .unwrap();
        for advertiser in &mut waiting {
            assert!(
                advertiser.ready().now_or_never().is_none(),
                "partial discovery cannot acknowledge the captured batch"
            );
        }
        // Cancel one captured caller, then enqueue its replacement. The old batch remains full,
        // so the next pass cannot capture the replacement before finishing the discovery burst.
        drop(waiting.pop());
        let mut late = advertiser.clone();
        assert!(late.ready().now_or_never().is_none());
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let connection = tokio::spawn(connection);
        let hash = block::Hash([24; 32]);
        let advert = late
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        let (delivery, message) = tokio::join!(advert, messages.next());
        connection.abort();
        assert_eq!(delivery.unwrap(), Response::Nil);
        assert_eq!(
            message,
            Some(Message::Inv(vec![InventoryHash::Block(hash)]))
        );
    })
    .await
    .expect("a waiter behind the retained batch must trigger another idle maintenance pass");
}

/// Bootstrap maintenance admits the first peer without an ordinary request; background handles
/// and advertiser clones must not keep its worker alive after the owning service is dropped.
#[tokio::test(start_paused = true)]
async fn advertiser_bootstraps_discovery_and_does_not_own_network_shutdown() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        let (client, _harness, connection, mut messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let (mut discovery, discovered) = mpsc::channel::<
            Result<Change<PeerSocketAddr, crate::peer::LoadTrackedClient>, BoxError>,
        >(1);
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .build();
        let mut advertiser = peer_set.block_advertiser();
        let (network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let background = network.background_service();
        let connection = tokio::spawn(connection);
        tokio::task::yield_now().await;
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        discovery
            .try_send(Ok(Change::Insert(addr, client.into())))
            .unwrap();

        let hash = block::Hash([20; 32]);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        let (delivery, message) = tokio::join!(advert, messages.next());
        assert_eq!(delivery.unwrap(), Response::Nil);
        assert_eq!(
            message,
            Some(Message::Inv(vec![InventoryHash::Block(hash)]))
        );

        drop(network);
        let (request, ready, closed) = tokio::join!(
            background.oneshot(Request::Peers),
            advertiser.ready(),
            messages.next(),
        );
        assert!(
            request.is_err(),
            "non-owning background handles must fail after shutdown"
        );
        assert!(
            ready.is_err(),
            "a maintenance barrier must observe worker shutdown"
        );
        assert_eq!(
            closed, None,
            "the worker releases its clients while the runtime is alive"
        );
        connection.await.unwrap();
    })
    .await
    .expect("bootstrap discovery and owner-driven shutdown must remain bounded");
}

/// Zero-peer readiness sleeps until discovery or owner-driven shutdown wakes its retained barrier.
#[tokio::test(start_paused = true)]
async fn block_advertiser_waits_for_eligible_peer_or_shutdown() {
    let _init_guard = zebra_test::init();
    for shutdown in [false, true] {
        let (mut discovery, discovered) =
            mpsc::channel::<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>>(1);
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .build();
        let mut advertiser = peer_set.block_advertiser();
        let (network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let ready = tokio::spawn(async move { advertiser.ready().await.map(|_| ()) });

        // The readiness task has no timer and is never polled manually by this test.
        tokio::time::sleep(REQUEST_TIMEOUT * 2).await;
        assert!(
            !ready.is_finished(),
            "zero eligible peers must keep readiness pending"
        );
        let _harness = if shutdown {
            drop(network);
            None
        } else {
            let (client, harness) = ClientTestHarness::build()
                .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
                .finish();
            let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
            discovery
                .try_send(Ok(Change::Insert(addr, client.into())))
                .unwrap();
            Some(harness)
        };
        let result = timeout(Duration::from_secs(1), ready)
            .await
            .expect("discovery and shutdown must wake advertiser readiness")
            .expect("readiness must not panic");
        assert_eq!(result.is_err(), shutdown);
    }
}

/// A dropped maintenance acknowledgment fails once, without leaving a completed receiver to poll.
#[tokio::test]
async fn block_advertiser_can_retry_after_maintenance_failure() {
    let _init_guard = zebra_test::init();
    let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(stream::pending::<
            Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>,
        >())
        .with_minimum_peer_version(minimum)
        .build();
    let mut advertiser = peer_set.block_advertiser();

    assert!(advertiser.ready().now_or_never().is_none());
    drop(peer_set.maintenance_rx.try_recv().unwrap());
    assert!(advertiser.ready().now_or_never().unwrap().is_err());

    assert!(
        advertiser.ready().now_or_never().is_none(),
        "polling after a failed acknowledgment must submit a fresh maintenance request"
    );
    peer_set
        .maintenance_rx
        .try_recv()
        .unwrap()
        .send(())
        .unwrap();
    assert!(advertiser.ready().now_or_never().unwrap().is_ok());
}

/// Independent maintenance still applies duplicate-address and per-IP admission limits.
#[tokio::test(start_paused = true)]
async fn advert_maintenance_does_not_register_rejected_discoveries() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        for duplicate in [false, true] {
            let (old_client, _old_harness, old_connection, mut old_messages) =
                real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
            let (new_client, _new_harness, new_connection, mut new_messages) =
                real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
            let (mut discovery, discovered) = mpsc::channel::<
                Result<Change<PeerSocketAddr, crate::peer::LoadTrackedClient>, BoxError>,
            >(1);
            let old_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
            let new_addr =
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), if duplicate { 1 } else { 2 }).into();
            discovery
                .try_send(Ok(Change::Insert(old_addr, old_client.into())))
                .unwrap();
            let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
            let (peer_set, _guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum)
                .max_conns_per_ip(1)
                .build();
            let mut advertiser = peer_set.block_advertiser();
            let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
            let old_task = tokio::spawn(old_connection);
            let new_task = tokio::spawn(new_connection);
            let hash = block::Hash([21; 32]);
            let first = advertiser
                .ready()
                .await
                .unwrap()
                .call(Request::AdvertiseBlockToAll(hash));
            let (delivery, message) = tokio::join!(first, old_messages.next());
            assert_eq!(delivery.unwrap(), Response::Nil);
            assert_eq!(
                message,
                Some(Message::Inv(vec![InventoryHash::Block(hash)]))
            );

            discovery
                .try_send(Ok(Change::Insert(new_addr, new_client.into())))
                .unwrap();
            let advert = advertiser
                .ready()
                .await
                .unwrap()
                .call(Request::AdvertiseBlockToAll(hash));
            let (delivery, existing, rejected) =
                tokio::join!(advert, old_messages.next(), new_messages.next());
            assert_eq!(delivery.unwrap(), Response::Nil);
            assert_eq!(
                existing,
                Some(Message::Inv(vec![InventoryHash::Block(hash)]))
            );
            assert_eq!(
                rejected, None,
                "unadmitted clients must be dropped without any advert"
            );
            old_task.abort();
            new_task.await.unwrap();
        }
    })
    .await
    .expect("admission checks must run independently of ordinary requests");
}

/// Connect a real Client and Connection, replacing only the remote wire with a bounded channel.
fn real_advert_peer(
    version: Version,
) -> (
    Client,
    ClientTestHarness,
    impl Future<Output = ()> + Send,
    mpsc::Receiver<Message>,
) {
    real_advert_peer_with_authorization(
        version,
        false,
        MinimumPeerVersion::new(zebra_chain::chain_tip::NoChainTip, &Network::Mainnet),
        watch::channel(crate::BanList::default()).1,
    )
}

fn real_advert_peer_with_authorization<C: zebra_chain::chain_tip::ChainTip + Send + 'static>(
    version: Version,
    early_block_relay: bool,
    minimum: MinimumPeerVersion<C>,
    bans: watch::Receiver<crate::BanList>,
) -> (
    Client,
    ClientTestHarness,
    impl Future<Output = ()> + Send,
    mpsc::Receiver<Message>,
) {
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let (mut client, harness) = ClientTestHarness::build()
        .with_version(version)
        .with_connected_addr(ConnectedAddr::InboundDirect { addr })
        .finish();
    let info = std::sync::Arc::make_mut(&mut client.connection_info);
    info.early_block_relay = early_block_relay;
    info.remote
        .services
        .set(PeerServices::NODE_EARLY_BLOCK_RELAY, early_block_relay);
    let (client_tx, client_rx) = mpsc::channel(1);
    client.server_tx = client_tx;
    let (advert_tx, advert_rx) = tokio::sync::mpsc::channel(4);
    client.block_advert_tx = advert_tx;
    let (wire_tx, wire_rx) = mpsc::channel(0);
    let connection = Connection::new(
        tower::service_fn(|_: Request| async { Ok::<_, BoxError>(Response::Nil) }),
        (client_rx, advert_rx, minimum, bans),
        client.error_slot.clone(),
        wire_tx.sink_map_err(|_| {
            SerializationError::from(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test remote closed its wire receiver",
            ))
        }),
        ActiveConnectionCounter::new_counter().track_connection(),
        client.connection_info.clone(),
        Vec::new(),
    );
    (client, harness, connection.run(stream::pending()), wire_rx)
}

/// Early inventory reaches a busy negotiated peer, not a non-negotiated sidecar.
#[tokio::test(start_paused = true)]
async fn early_block_advert_restricts_delivery_to_negotiated_peers() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(2), async {
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (early, _early_harness, early_connection, mut early_messages) =
            real_advert_peer_with_authorization(
                CURRENT_NETWORK_PROTOCOL_VERSION,
                true,
                minimum.clone(),
                watch::channel(crate::BanList::default()).1,
            );
        let (mut legacy, _legacy_harness, legacy_connection, mut legacy_messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let early_addr: PeerSocketAddr = "127.0.0.1:1".parse().unwrap();
        let legacy_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
        let legacy_info = std::sync::Arc::make_mut(&mut legacy.connection_info);
        legacy_info.connected_addr = ConnectedAddr::new_inbound_direct(legacy_addr);
        // A remote service signal alone must not authorize delivery.
        legacy_info
            .remote
            .services
            .insert(PeerServices::NODE_EARLY_BLOCK_RELAY);
        let discover = stream::iter([
            Ok::<_, BoxError>(Change::Insert(early_addr, early.into())),
            Ok(Change::Insert(legacy_addr, legacy.into())),
        ])
        .chain(stream::pending());
        let (mut peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discover)
            .with_minimum_peer_version(minimum)
            .with_block_gossip_peer_ips(vec![legacy_addr.ip()])
            .build();
        peer_set.ready().await.unwrap();
        let early_task = tokio::spawn(early_connection);
        let legacy_task = tokio::spawn(legacy_connection);
        assert_eq!(
            early_messages.next().await,
            Some(Message::SendCompact {
                high_bandwidth: true,
                version: 1
            })
        );
        let mut busy = peer_set.take_ready_service(&early_addr).unwrap();
        let _pending_request = busy.call(Request::Peers);
        peer_set.push_unready(early_addr, busy);
        assert_eq!(early_messages.next().await, Some(Message::GetAddr));

        let mut advertiser = peer_set.block_advertiser();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let hash = block::Hash([25; 32]);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseEarlyBlock(hash));
        let (result, message) = tokio::join!(advert, early_messages.next());
        assert_eq!(result.unwrap(), Response::Nil);
        assert_eq!(
            message,
            Some(Message::Inv(vec![InventoryHash::Block(hash)]))
        );
        assert!(legacy_messages.try_recv().is_err());

        // Committed relay still includes the legacy peer, while the early peer stays busy.
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        let (result, early, legacy) =
            tokio::join!(advert, early_messages.next(), legacy_messages.next());
        assert_eq!(result.unwrap(), Response::Nil);
        assert_eq!(early, Some(Message::Inv(vec![InventoryHash::Block(hash)])));
        assert_eq!(legacy, Some(Message::Inv(vec![InventoryHash::Block(hash)])));
        early_task.abort();
        legacy_task.abort();
    })
    .await
    .expect("early inventory must bypass a busy request without reaching legacy peers");
}

/// Channel admission is not evidence that the connection has written an inventory message.
#[tokio::test(start_paused = true)]
async fn block_advert_success_waits_for_peer_write() {
    let _init_guard = zebra_test::init();
    let (client, _harness, connection, mut messages) =
        real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
        .chain(stream::pending());
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discover)
        .with_minimum_peer_version(minimum_peer_version)
        .build();
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let hash = block::Hash([14; 32]);
    let mut advertiser = peer_set.block_advertiser();
    let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    let mut advert = Box::pin(
        advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash)),
    );
    assert!(
        advert.as_mut().now_or_never().is_none(),
        "an unpolled connection cannot have acknowledged a peer write"
    );
    let mut connection = Box::pin(connection);
    assert!(connection.as_mut().now_or_never().is_none());
    assert_eq!(
        messages.try_recv().unwrap(),
        Message::Inv(vec![InventoryHash::Block(hash)])
    );
    assert!(connection.as_mut().now_or_never().is_none());
    assert_eq!(
        timeout(Duration::from_secs(1), advert)
            .await
            .unwrap()
            .unwrap(),
        Response::Nil
    );
}

async fn failed_block_advert_write(all_peers: bool) {
    let (client, harness, connection, messages) =
        real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
        .chain(stream::pending());
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discover)
        .with_minimum_peer_version(minimum_peer_version)
        .build();
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let hash = block::Hash([15; 32]);
    let request = if all_peers {
        Request::AdvertiseBlockToAll(hash)
    } else {
        Request::AdvertiseBlock(hash, None)
    };
    let mut advertiser = peer_set.block_advertiser();
    let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    let advert = advertiser.ready().await.unwrap().call(request);
    drop(messages);
    let (result, ()) = timeout(Duration::from_secs(1), async {
        tokio::join!(advert, connection)
    })
    .await
    .expect("the closed wire must fail the actual connection write");
    assert!(
        harness.current_error().is_some(),
        "the connection detected the write failure"
    );
    assert!(
        result.is_err(),
        "zero successful peer writes must not be reported as success"
    );
}

/// Committed adverts must propagate actual connection write failures.
#[tokio::test(start_paused = true)]
async fn committed_block_advert_reports_peer_write_failure() {
    let _init_guard = zebra_test::init();
    failed_block_advert_write(true).await;
}

/// Sampled tip/early adverts must not discard every selected peer's write error.
#[tokio::test(start_paused = true)]
async fn ordinary_block_advert_reports_peer_write_failure() {
    let _init_guard = zebra_test::init();
    failed_block_advert_write(false).await;
}

/// Admission to a full or closed real connection queue is not a successful local write.
#[tokio::test(start_paused = true)]
async fn block_advert_reports_full_and_closed_connection_queues() {
    let _init_guard = zebra_test::init();
    for close in [false, true] {
        let (client, _harness, connection, _messages) =
            real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
        let capacity = client.block_advert_tx.max_capacity();
        let mut connection = Some(connection);
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
            .chain(stream::pending());
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (mut peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discover)
            .with_minimum_peer_version(minimum)
            .build();
        timeout(Duration::from_secs(1), peer_set.ready())
            .await
            .unwrap()
            .unwrap();
        let mut advertiser = peer_set.block_advertiser();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let request = Request::AdvertiseBlockToAll(block::Hash([17; 32]));
        if close {
            advertiser.ready().await.unwrap();
            drop(connection.take());
        } else {
            // Cancel callers without polling the connection: their bounded envelopes remain queued.
            for _ in 0..capacity {
                drop(advertiser.ready().await.unwrap().call(request.clone()));
            }
            advertiser.ready().await.unwrap();
        }
        assert!(timeout(Duration::from_secs(1), advertiser.call(request))
            .await
            .unwrap()
            .is_err());
    }
}

/// Clones share the existing network concurrency bound, and cancellation returns its permit.
#[tokio::test(start_paused = true)]
async fn block_advertiser_bounds_in_flight_writes_across_clones() {
    let _init_guard = zebra_test::init();
    let (client, _harness, _connection, _messages) =
        real_advert_peer(CURRENT_NETWORK_PROTOCOL_VERSION);
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
        .chain(stream::pending());
    let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discover)
        .with_minimum_peer_version(minimum)
        .build();
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let mut advertiser = peer_set.block_advertiser();
    let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    let mut pending = Vec::new();
    for _ in 0..PEERSET_BUFFER_SIZE {
        let mut clone = advertiser.clone();
        pending.push(
            clone
                .ready()
                .await
                .unwrap()
                .call(Request::AdvertiseBlockToAll(block::Hash([18; 32]))),
        );
    }
    assert!(advertiser.ready().now_or_never().is_none());
    drop(pending.pop());
    timeout(Duration::from_secs(1), advertiser.ready())
        .await
        .unwrap()
        .unwrap();
}

async fn queued_block_advert_revocation(upgrade: bool, busy: bool) {
    let network = Network::Mainnet;
    let version = Version::min_specified_for_upgrade(&network, NetworkUpgrade::Nu6_2);
    let (minimum_peer_version, best_tip) = MinimumPeerVersion::with_mock_chain_tip(&network);
    let (bans_tx, bans_rx) = watch::channel(crate::BanList::default());
    let (client, _harness, connection, mut messages) = real_advert_peer_with_authorization(
        version,
        false,
        minimum_peer_version.clone(),
        bans_rx.clone(),
    );
    let mut connection = Box::pin(connection);
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let discover = stream::iter([Ok::<_, BoxError>(Change::Insert(addr, client.into()))])
        .chain(stream::pending());
    best_tip.send_best_tip_height(NetworkUpgrade::Nu6_2.activation_height(&network));
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discover)
        .with_minimum_peer_version(minimum_peer_version)
        .build();
    peer_set.bans_receiver = bans_rx;
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let _pending_request = if busy {
        let request = peer_set.call(Request::Peers);
        assert!(connection.as_mut().now_or_never().is_none());
        assert_eq!(messages.try_recv().unwrap(), Message::GetAddr);
        assert!(connection.as_mut().now_or_never().is_none());
        Some(request)
    } else {
        None
    };
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .unwrap()
        .unwrap();
    let mut advertiser = peer_set.block_advertiser();
    let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    let advert = advertiser
        .ready()
        .await
        .unwrap()
        .call(Request::AdvertiseBlockToAll(block::Hash([16; 32])));

    // The connection is deliberately not polled between admission and revocation.
    if upgrade {
        best_tip.send_best_tip_height(NetworkUpgrade::Nu6_3.activation_height(&network));
    } else {
        bans_tx.send_modify(|bans| {
            bans.ban(Ipv4Addr::LOCALHOST.into());
        });
    }
    let _completion = connection.as_mut().now_or_never();
    assert!(
        messages.try_recv().is_err(),
        "queued adverts must recheck authorization before writing to the peer"
    );
    assert!(timeout(Duration::from_secs(1), advert)
        .await
        .unwrap()
        .is_err());
}

/// Idle connections must revoke adverts queued before a ban.
#[tokio::test(start_paused = true)]
async fn queued_block_advert_is_revoked_by_ban() {
    let _init_guard = zebra_test::init();
    queued_block_advert_revocation(false, false).await;
}

/// The enqueue-time minimum-version check alone cannot authorize a later write.
#[tokio::test(start_paused = true)]
async fn queued_block_advert_is_revoked_by_upgrade() {
    let _init_guard = zebra_test::init();
    queued_block_advert_revocation(true, false).await;
}

/// Waiting for an ordinary response must not defer a queued advert's ban check.
#[tokio::test(start_paused = true)]
async fn queued_block_advert_is_revoked_by_ban_while_awaiting_response() {
    let _init_guard = zebra_test::init();
    queued_block_advert_revocation(false, true).await;
}

/// An upgrade revokes queued adverts even while an ordinary response remains outstanding.
#[tokio::test(start_paused = true)]
async fn queued_block_advert_is_revoked_by_upgrade_while_awaiting_response() {
    let _init_guard = zebra_test::init();
    queued_block_advert_revocation(true, true).await;
}

/// Upgrades must exclude obsolete peers from adverts even while their services remain busy.
#[tokio::test(start_paused = true)]
async fn block_adverts_skip_busy_outdated_peers() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let peer_versions = PeerVersions {
        peer_versions: vec![
            Version::min_specified_for_upgrade(&network, NetworkUpgrade::Nu6_2),
            Version::min_specified_for_upgrade(&network, NetworkUpgrade::Nu6_3),
        ],
    };
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, best_tip) = MinimumPeerVersion::with_mock_chain_tip(&network);
    best_tip.send_best_tip_height(NetworkUpgrade::Nu6_2.activation_height(&network));
    let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
        .with_discover(discovered_peers)
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
        .build();

    timeout(Duration::from_secs(1), async {
        peer_set.ready().await.unwrap();
        let busy_addr: PeerSocketAddr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
        let mut busy_service = peer_set.take_ready_service(&busy_addr).unwrap();
        let _first_request = busy_service.call(Request::Peers);
        busy_service.ready().await.unwrap();
        let _second_request = busy_service.call(Request::Peers);
        assert!(busy_service.ready().now_or_never().is_none());
        peer_set.push_unready(busy_addr, busy_service);

        let before_upgrade = block::Hash([11; 32]);
        let mut advertiser = peer_set.block_advertiser();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(before_upgrade));
        for handle in &mut handles {
            assert_eq!(handle.receive_block_advert(), before_upgrade);
        }
        advert.await.unwrap();

        best_tip.send_best_tip_height(NetworkUpgrade::Nu6_3.activation_height(&network));
        let after_upgrade = block::Hash([12; 32]);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(after_upgrade));
        assert!(
            !handles[0].wants_connection_heartbeats(),
            "the obsolete peer is disconnected"
        );
        assert!(handles[0].block_advert_receiver.try_recv().is_err());
        assert_eq!(handles[1].receive_block_advert(), after_upgrade);
        advert.await.unwrap();
    })
    .await
    .expect("the supported peer permits dispatch while the obsolete peer remains busy");
}

/// Out-of-band adverts must respect bans and discovery removals while ordinary traffic is idle.
#[tokio::test(start_paused = true)]
async fn block_adverts_skip_banned_and_removed_peers() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(1), async {
        let peer_versions = PeerVersions {
            peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION; 2],
        };
        let (clients, mut handles) = peer_versions.mock_peers();
        let addresses: [PeerSocketAddr; 2] = [
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into(),
            SocketAddr::new(Ipv4Addr::new(127, 0, 0, 2).into(), 2).into(),
        ];
        let (mut changes, changes_rx) = mpsc::channel(1);
        let discovered = stream::iter(
            addresses
                .into_iter()
                .zip(clients)
                .map(|(address, client)| Ok::<_, BoxError>(Change::Insert(address, client))),
        )
        .chain(changes_rx);
        let (minimum, _best_tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let (mut peer_set, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum)
            .build();
        peer_set.ready().await.unwrap();
        let (bans, bans_rx) = watch::channel(crate::BanList::default());
        peer_set.bans_receiver = bans_rx;
        let mut advertiser = peer_set.block_advertiser();
        let mut advert_peers = peer_set.block_advert_senders.subscribe();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);

        bans.send_modify(|bans| {
            bans.ban(addresses[0].ip());
        });
        let hash = block::Hash([10; 32]);
        let advert = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlockToAll(hash));
        assert!(handles[0].block_advert_receiver.try_recv().is_err());
        assert!(!handles[0].wants_connection_heartbeats());
        assert_eq!(handles[1].receive_block_advert(), hash);
        advert.await.unwrap();

        advertiser.ready().await.unwrap();
        changes.try_send(Ok(Change::Remove(addresses[1]))).unwrap();
        advert_peers
            .wait_for(|peers| peers.is_empty())
            .await
            .unwrap();
        assert!(advertiser
            .call(Request::AdvertiseBlockToAll(hash))
            .await
            .is_err());
        assert!(handles[1].block_advert_receiver.try_recv().is_err());
        assert!(!handles[1].wants_connection_heartbeats());
    })
    .await
    .expect("revocation and removal must not require ordinary ready peers");
}

#[test]
fn remove_unready_peer_clears_cancel_handle_and_updates_counts() {
    let peer_versions = PeerVersions {
        peer_versions: vec![Version::min_specified_for_upgrade(
            &Network::Mainnet,
            NetworkUpgrade::Nu6_2,
        )],
    };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (discovered_peers, _handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Prepare a banned IP map (not strictly required for remove(), but keeps
        // the test's setup similar to real-world conditions).
        let banned_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let mut bans = crate::BanList::default();
        bans.ban(banned_ip);
        let (_bans_tx, bans_rx) = watch::channel(bans);
        peer_set.bans_receiver = bans_rx;

        // Create a cancel handle as if a request was in-flight to `banned_addr`.
        let banned_addr: PeerSocketAddr = SocketAddr::new(banned_ip, 1).into();
        let (tx, _rx) =
            crate::peer_set::set::oneshot::channel::<crate::peer_set::set::CancelClientWork>();
        peer_set.cancel_handles.insert(banned_addr, tx);

        // The peer is counted as 1 peer with that IP.
        assert_eq!(peer_set.num_peers_with_ip(banned_ip), 1);

        // Remove the peer (simulates a discovery::Remove or equivalent).
        peer_set.remove(&banned_addr);

        // After removal, the cancel handle should be gone and the count zero.
        assert!(!peer_set.cancel_handles.contains_key(&banned_addr));
        assert_eq!(peer_set.num_peers_with_ip(banned_ip), 0);
    });
}

/// Check that a peer set routes inventory requests to a peer that has advertised that inventory.
#[test]
fn peer_set_route_inv_advertised_registry() {
    peer_set_route_inv_advertised_registry_order(true);
    peer_set_route_inv_advertised_registry_order(false);
}

fn peer_set_route_inv_advertised_registry_order(advertised_first: bool) {
    let test_hash = block::Hash([0; 32]);
    let test_inv = InventoryHash::Block(test_hash);

    // Hard-code the fixed test address created by mock_peer_discovery
    // TODO: add peer test addresses to ClientTestHarness
    let test_peer = if advertised_first {
        "127.0.0.1:1"
    } else {
        "127.0.0.1:2"
    }
    .parse()
    .expect("unexpected invalid peer address");

    let test_change = InventoryStatus::new_available(test_inv, test_peer);

    // Use two peers with the same version
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get peers and client handles of them
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), 2);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Advertise some inventory
        peer_set_guard
            .inventory_sender()
            .as_mut()
            .expect("unexpected missing inv sender")
            .send(test_change)
            .expect("unexpected dropped receiver");

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(peer_ready.ready_services.len(), 2);

        // Send an inventory-based request
        let sent_request = Request::BlocksByHash(iter::once(test_hash).collect());
        let _fut = peer_ready.call(sent_request.clone());

        // Check that the client that advertised the inventory received the request
        let advertised_handle = if advertised_first {
            &mut handles[0]
        } else {
            &mut handles[1]
        };

        if let Some(ClientRequest { request, .. }) = advertised_handle
            .try_to_receive_outbound_client_request()
            .request()
        {
            assert_eq!(sent_request, request);
        } else {
            panic!("inv request not routed to advertised peer");
        }

        let other_handle = if advertised_first {
            &mut handles[1]
        } else {
            &mut handles[0]
        };

        assert!(
            other_handle
                .try_to_receive_outbound_client_request()
                .request()
                .is_none(),
            "request routed to non-advertised peer",
        );
    });
}

/// Check that a peer set routes inventory requests to peers that are not missing that inventory.
#[test]
fn peer_set_route_inv_missing_registry() {
    peer_set_route_inv_missing_registry_order(true);
    peer_set_route_inv_missing_registry_order(false);
}

fn peer_set_route_inv_missing_registry_order(missing_first: bool) {
    let test_hash = block::Hash([0; 32]);
    let test_inv = InventoryHash::Block(test_hash);

    // Hard-code the fixed test address created by mock_peer_discovery
    // TODO: add peer test addresses to ClientTestHarness
    let test_peer = if missing_first {
        "127.0.0.1:1"
    } else {
        "127.0.0.1:2"
    }
    .parse()
    .expect("unexpected invalid peer address");

    let test_change = InventoryStatus::new_missing(test_inv, test_peer);

    // Use two peers with the same version
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get peers and client handles of them
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), 2);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Mark some inventory as missing
        peer_set_guard
            .inventory_sender()
            .as_mut()
            .expect("unexpected missing inv sender")
            .send(test_change)
            .expect("unexpected dropped receiver");

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(peer_ready.ready_services.len(), 2);

        // Send an inventory-based request
        let sent_request = Request::BlocksByHash(iter::once(test_hash).collect());
        let _fut = peer_ready.call(sent_request.clone());

        // Check that the client missing the inventory did not receive the request
        let missing_handle = if missing_first {
            &mut handles[0]
        } else {
            &mut handles[1]
        };

        assert!(
            missing_handle
                .try_to_receive_outbound_client_request()
                .request()
                .is_none(),
            "request routed to missing peer",
        );

        // Check that the client that was not missing the inventory received the request
        let other_handle = if missing_first {
            &mut handles[1]
        } else {
            &mut handles[0]
        };

        if let Some(ClientRequest { request, .. }) = other_handle
            .try_to_receive_outbound_client_request()
            .request()
        {
            assert_eq!(sent_request, request);
        } else {
            panic!(
                "inv request should have been routed to the only peer not missing the inventory"
            );
        }
    });
}

/// Check that a peer set fails inventory requests if all peers are missing that inventory.
#[test]
fn peer_set_route_inv_all_missing_fail() {
    let test_hash = block::Hash([0; 32]);
    let test_inv = InventoryHash::Block(test_hash);

    // Hard-code the fixed test address created by mock_peer_discovery
    // TODO: add peer test addresses to ClientTestHarness
    let test_peer = "127.0.0.1:1"
        .parse()
        .expect("unexpected invalid peer address");

    let test_change = InventoryStatus::new_missing(test_inv, test_peer);

    // Use one peer
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Pause the runtime's timer so that it advances automatically.
    //
    // CORRECTNESS: This test does not depend on external resources that could really timeout, like
    // real network connections.
    tokio::time::pause();

    // Get the peer and its client handle
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Make sure we have the right number of peers
    assert_eq!(handles.len(), 1);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .build();

        // Mark the inventory as missing for all peers
        peer_set_guard
            .inventory_sender()
            .as_mut()
            .expect("unexpected missing inv sender")
            .send(test_change)
            .expect("unexpected dropped receiver");

        // Get peerset ready
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        // Check we have the right amount of ready services
        assert_eq!(peer_ready.ready_services.len(), 1);

        // Send an inventory-based request
        let sent_request = Request::BlocksByHash(iter::once(test_hash).collect());
        let response_fut = peer_ready.call(sent_request.clone());

        // Check that the client missing the inventory did not receive the request
        let missing_handle = &mut handles[0];

        assert!(
            missing_handle
                    .try_to_receive_outbound_client_request()
                    .request().is_none(),
            "request routed to missing peer",
        );

        // Check that the response is a synthetic error
        let response = response_fut.await;
        assert_eq!(
            response
                .expect_err("peer set should return an error (not a Response)")
                .downcast_ref::<SharedPeerError>()
                .expect("peer set should return a boxed SharedPeerError")
                .inner_debug(),
            "NotFoundRegistry([Block(block::Hash(\"0000000000000000000000000000000000000000000000000000000000000000\"))])"
        );
    });
}

/// Send one request through the peer set and answer it using the expected connection.
async fn respond_to_request(
    peer_set: &mut impl Service<Request, Response = Response, Error = BoxError>,
    handle: &mut ClientTestHarness,
    request: Request,
    response: Result<Response, SharedPeerError>,
) -> Result<Response, BoxError> {
    let response_fut = timeout(Duration::from_secs(10), peer_set.ready())
        .await
        .expect("peer readiness must not hang")
        .expect("peer set is ready")
        .call(request.clone());
    let client_request = handle
        .try_to_receive_outbound_client_request()
        .request()
        .expect("the expected peer received the request");
    assert_eq!(client_request.request, request);
    client_request
        .tx
        .send(response)
        .expect("the request is still waiting for its response");
    timeout(Duration::from_secs(10), response_fut)
        .await
        .expect("the response must not hang")
}

/// Low or frozen handshake heights must not exempt failed discovery requests.
#[test]
fn find_failures_disconnect_despite_low_or_frozen_handshake_height() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    for (remote_height, local_height, request) in [
        (
            block::Height(0),
            None,
            Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
        ),
        (
            block::Height(2_500_000),
            Some(block::Height(2_490_000)),
            Request::FindHeaders {
                known_blocks: vec![],
                stop: None,
            },
        ),
    ] {
        let (discovered_peers, mut handle) = mock_peer_discovery_with_start_height(remote_height);
        let (minimum_peer_version, best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        best_tip.send_best_tip_height(local_height);

        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .build();

            for failure in 1..=FIND_RESPONSE_STALL_THRESHOLD {
                if failure == FIND_RESPONSE_STALL_THRESHOLD {
                    // The local tip overtakes the frozen handshake height.
                    best_tip.send_best_tip_height(Some(block::Height(remote_height.0 + 1)));
                }
                assert!(respond_to_request(
                    &mut peer_set,
                    &mut handle,
                    request.clone(),
                    Err(PeerError::ConnectionReceiveTimeout.into()),
                )
                .await
                .is_err());
                let _ = peer_set.ready().now_or_never();
                assert_eq!(
                    handle.wants_connection_heartbeats(),
                    failure < FIND_RESPONSE_STALL_THRESHOLD,
                    "only the threshold failure must disconnect the peer",
                );
            }
        });
    }
}

/// A caught-up peer can legitimately stay silent until the syncer cancels its request.
#[test]
fn cancelled_find_requests_retain_silent_connections() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let (discovered_peers, mut handle) =
        mock_peer_discovery_with_start_height(block::Height(2_500_000));
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        for attempt in 0..=FIND_RESPONSE_STALL_THRESHOLD {
            let request = if attempt % 2 == 0 {
                Request::FindHeaders {
                    known_blocks: vec![],
                    stop: None,
                }
            } else {
                Request::FindBlocks {
                    known_blocks: vec![],
                    stop: None,
                }
            };
            let response_fut = timeout(Duration::from_secs(10), peer_set.ready())
                .await
                .expect("peer readiness must not hang")
                .expect("peer set is ready")
                .call(request.clone());
            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");
            assert_eq!(client_request.request, request);
            assert!(timeout(Duration::from_secs(6), response_fut).await.is_err());
            let _ = peer_set.ready().now_or_never();
            assert!(
                handle.wants_connection_heartbeats(),
                "caller cancellation must not treat an honest silent peer as failed",
            );
        }
    });
}

/// Both nonempty and empty successes reset failures, even across local tip changes.
#[test]
fn find_successes_reset_failures_across_tip_changes() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .unwrap();

    for (request, nonempty, empty) in [
        (
            Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            Response::BlockHashes(vec![block::Hash::from(&block)]),
            Response::BlockHashes(vec![]),
        ),
        (
            Request::FindHeaders {
                known_blocks: vec![],
                stop: None,
            },
            Response::BlockHeaders(vec![block::CountedHeader {
                header: block.header.clone(),
            }]),
            Response::BlockHeaders(vec![]),
        ),
    ] {
        let (discovered_peers, mut handle) =
            mock_peer_discovery_with_start_height(block::Height(2_500_000));
        let (minimum_peer_version, best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        best_tip.send_best_tip_height(Some(block::Height(2_490_000)));

        runtime.block_on(async move {
            let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .build();

            for (index, response) in [nonempty, empty].into_iter().enumerate() {
                for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
                    assert!(respond_to_request(
                        &mut peer_set,
                        &mut handle,
                        request.clone(),
                        Err(PeerError::ConnectionReceiveTimeout.into()),
                    )
                    .await
                    .is_err());
                }
                let _ = peer_set.ready().now_or_never();
                assert!(handle.wants_connection_heartbeats());

                if index == 1 {
                    // An empty response when the local tip overtakes the peer must still reset.
                    best_tip.send_best_tip_height(Some(block::Height(2_500_001)));
                }
                assert_eq!(
                    respond_to_request(
                        &mut peer_set,
                        &mut handle,
                        request.clone(),
                        Ok(response.clone()),
                    )
                    .await
                    .expect("find response succeeds"),
                    response,
                );
                best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
            }

            // Success resets the counter, rather than disabling failure tracking.
            for failure in 1..=FIND_RESPONSE_STALL_THRESHOLD {
                assert!(respond_to_request(
                    &mut peer_set,
                    &mut handle,
                    request.clone(),
                    Err(PeerError::ConnectionReceiveTimeout.into()),
                )
                .await
                .is_err());
                let _ = peer_set.ready().now_or_never();
                assert_eq!(
                    handle.wants_connection_heartbeats(),
                    failure < FIND_RESPONSE_STALL_THRESHOLD,
                );
            }
        });
    }
}

/// Empty find replies are healthy even when a stale tip estimate or rollback says we lag.
#[test]
fn empty_find_responses_retain_connections_across_tip_changes() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let (discovered_peers, mut handle) =
        mock_peer_discovery_with_start_height(block::Height(2_500_000));
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        for (local_height, estimated_distance) in [
            (Some(block::Height(2_500_001)), 0),
            (Some(block::Height(2_490_000)), 100_000),
            (None, 100_000),
        ] {
            best_tip.send_best_tip_height(local_height);
            best_tip.send_estimated_distance_to_network_chain_tip(Some(estimated_distance));
            for request_index in 0..=FIND_RESPONSE_STALL_THRESHOLD {
                let (request, response) = if request_index % 2 == 0 {
                    (
                        Request::FindBlocks {
                            known_blocks: vec![],
                            stop: None,
                        },
                        Response::BlockHashes(vec![]),
                    )
                } else {
                    (
                        Request::FindHeaders {
                            known_blocks: vec![],
                            stop: None,
                        },
                        Response::BlockHeaders(vec![]),
                    )
                };
                respond_to_request(&mut peer_set, &mut handle, request, Ok(response))
                    .await
                    .expect("empty find response succeeds");
                let _ = peer_set.ready().now_or_never();
                assert!(
                    handle.wants_connection_heartbeats(),
                    "empty responses must not disconnect a healthy peer",
                );
            }
        }
    });
}

/// Fast empty replies must not monopolize discovery, and reconnecting cannot jump the queue.
#[test]
fn find_requests_rotate_past_fast_empty_responder_and_reconnections() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let peer_versions = PeerVersions {
        peer_versions: vec![CURRENT_NETWORK_PROTOCOL_VERSION; 2],
    };
    let (mut clients, mut handles) = peer_versions.mock_peers();
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        // Give the second connection a higher observed RTT. With two peers, P2C would
        // deterministically select the first peer on every subsequent request.
        let slow_response = clients[1]
            .ready()
            .await
            .expect("mock client is ready")
            .call(Request::Peers);
        let slow_request = handles[1]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("slow peer received the warmup request");
        tokio::time::advance(Duration::from_secs(60)).await;
        slow_request
            .tx
            .send(Ok(Response::Peers(vec![])))
            .expect("warmup request is pending");
        slow_response.await.expect("warmup response succeeds");

        let addresses = [
            PeerSocketAddr::from(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1)),
            PeerSocketAddr::from(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 2)),
        ];
        let (mut discovery_tx, discovered_peers) = futures::channel::mpsc::channel(4);
        for (address, client) in addresses.into_iter().zip(clients) {
            discovery_tx
                .try_send(Ok::<_, BoxError>(Change::Insert(address, client)))
                .expect("discovery channel has room");
        }
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let requests = [
            (
                Request::FindBlocks {
                    known_blocks: vec![],
                    stop: None,
                },
                Response::BlockHashes(vec![]),
            ),
            (
                Request::FindHeaders {
                    known_blocks: vec![],
                    stop: None,
                },
                Response::BlockHeaders(vec![]),
            ),
        ];

        for index in [0, 1, 0, 1] {
            let (request, response) = &requests[index];
            respond_to_request(
                &mut peer_set,
                &mut handles[index],
                request.clone(),
                Ok(response.clone()),
            )
            .await
            .expect("each continuously ready connection gets its turn");
        }

        // Fill the mock channel's buffer and sender slot. Its neighbor must remain usable
        // even when the unready peer reaches the front of the discovery order.
        let mut busy_responses = Vec::new();
        for _ in 0..2 {
            busy_responses.push(
                timeout(Duration::from_secs(10), peer_set.ready())
                    .await
                    .expect("peer readiness must not hang")
                    .expect("peer set is ready")
                    .call(requests[0].0.clone()),
            );
            respond_to_request(
                &mut peer_set,
                &mut handles[1],
                requests[1].0.clone(),
                Ok(requests[1].1.clone()),
            )
            .await
            .expect("the other peer gets its turn while requests accumulate");
        }
        respond_to_request(
            &mut peer_set,
            &mut handles[1],
            requests[1].0.clone(),
            Ok(requests[1].1.clone()),
        )
        .await
        .expect("an unready peer must not block its ready neighbor");
        for busy_response in busy_responses {
            handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("busy peer retained its request")
                .tx
                .send(Ok(requests[0].1.clone()))
                .expect("busy request is pending");
            timeout(Duration::from_secs(10), busy_response)
                .await
                .expect("busy response must not hang")
                .expect("busy response succeeds");
        }
        for index in [0, 1] {
            respond_to_request(
                &mut peer_set,
                &mut handles[index],
                requests[index].0.clone(),
                Ok(requests[index].1.clone()),
            )
            .await
            .expect("a newly ready peer keeps its place in discovery order");
        }

        // Silence is also legitimate: a caller timeout must move discovery to the next peer.
        let response_fut = timeout(Duration::from_secs(10), peer_set.ready())
            .await
            .expect("peer readiness must not hang")
            .expect("peer set is ready")
            .call(requests[0].0.clone());
        let silent_request = handles[0]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("first peer received the request");
        assert_eq!(silent_request.request, requests[0].0);
        assert!(timeout(Duration::from_secs(6), response_fut).await.is_err());
        respond_to_request(
            &mut peer_set,
            &mut handles[1],
            requests[1].0.clone(),
            Ok(requests[1].1.clone()),
        )
        .await
        .expect("a silent responder must not delay the next ready connection");
        assert!(handles[0].wants_connection_heartbeats());

        // The first connection is next, but replacing it creates a new identity:
        // the surviving connection must run before the replacement at the same address.
        let (replacement, replacement_handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .finish();
        discovery_tx
            .try_send(Ok(Change::Remove(addresses[0])))
            .expect("discovery channel has room");
        discovery_tx
            .try_send(Ok(Change::Insert(addresses[0], replacement.into())))
            .expect("discovery channel has room");
        let mut old_handle = std::mem::replace(&mut handles[0], replacement_handle);
        for index in [1, 0] {
            let (request, response) = &requests[index];
            respond_to_request(
                &mut peer_set,
                &mut handles[index],
                request.clone(),
                Ok(response.clone()),
            )
            .await
            .expect("new connections wait behind existing connections");
        }
        assert!(!old_handle.wants_connection_heartbeats());
        assert!(handles
            .iter_mut()
            .all(|handle| handle.wants_connection_heartbeats()));
    });
}

/// Completion events belong to a connection, not a reusable discovery address.
#[test]
fn find_results_from_replaced_connections_do_not_affect_replacement() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let address: PeerSocketAddr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1).into();
    let (client, mut old_handle) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_start_height(block::Height(2_500_000))
        .finish();
    let (mut discovery_tx, discovered_peers) = futures::channel::mpsc::channel(2);
    discovery_tx
        .try_send(Ok::<_, BoxError>(Change::Insert(address, client.into())))
        .expect("discovery channel has room");
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();
        let request = Request::FindBlocks {
            known_blocks: vec![],
            stop: None,
        };
        let mut pending = Vec::new();
        for _ in 0..=FIND_RESPONSE_STALL_THRESHOLD {
            let response_fut = timeout(Duration::from_secs(10), peer_set.ready())
                .await
                .expect("peer readiness must not hang")
                .expect("peer is ready")
                .call(request.clone());
            let client_request = old_handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("old connection received the request");
            pending.push((response_fut, client_request));
        }

        let (replacement, mut handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_start_height(block::Height(2_500_000))
            .finish();
        discovery_tx
            .try_send(Ok(Change::Remove(address)))
            .expect("discovery channel has room");
        discovery_tx
            .try_send(Ok(Change::Insert(address, replacement.into())))
            .expect("discovery channel has room");
        timeout(Duration::from_secs(10), peer_set.ready())
            .await
            .expect("replacement readiness must not hang")
            .expect("replacement is ready");
        assert!(!old_handle.wants_connection_heartbeats());

        let (late_success, late_success_request) = pending
            .pop()
            .expect("one old request is reserved for success");
        for (response_fut, client_request) in pending {
            client_request
                .tx
                .send(Err(PeerError::ConnectionReceiveTimeout.into()))
                .expect("old request is still waiting for its response");
            assert!(timeout(Duration::from_secs(10), response_fut)
                .await
                .expect("old response must not hang")
                .is_err());
            let _ = peer_set.ready().now_or_never();
            assert!(
                handle.wants_connection_heartbeats(),
                "old connection failures must not disconnect its replacement",
            );
        }

        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
            assert!(respond_to_request(
                &mut peer_set,
                &mut handle,
                request.clone(),
                Err(PeerError::ConnectionReceiveTimeout.into()),
            )
            .await
            .is_err());
        }
        late_success_request
            .tx
            .send(Ok(Response::BlockHashes(vec![])))
            .expect("old request is still waiting for its response");
        timeout(Duration::from_secs(10), late_success)
            .await
            .expect("old response must not hang")
            .expect("old response succeeds");

        assert!(respond_to_request(
            &mut peer_set,
            &mut handle,
            request,
            Err(PeerError::ConnectionReceiveTimeout.into()),
        )
        .await
        .is_err());
        let _ = peer_set.ready().now_or_never();
        assert!(
            !handle.wants_connection_heartbeats(),
            "old connection successes must not clear the replacement's failures",
        );
    });
}

/// Check that the sync stall detector does not disconnect the configured zcashd-compat sidecar.
#[test]
fn find_failures_do_not_disconnect_zcashd_compat() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    tokio::time::pause();
    let sidecar_ip = Ipv4Addr::LOCALHOST;
    let sidecar_addr: PeerSocketAddr =
        SocketAddr::new(IpAddr::V6(sidecar_ip.to_ipv6_mapped()), 1).into();
    let (sidecar, mut sidecar_handle) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_connected_addr(ConnectedAddr::new_inbound_direct(sidecar_addr))
        .with_start_height(block::Height(2_500_000))
        .finish();
    let discovered_peers = stream::iter([Ok::<_, BoxError>(Change::Insert(
        sidecar_addr,
        sidecar.into(),
    ))])
    .chain(stream::pending());
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
    best_tip.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::days(1));

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_block_gossip_peer_ips(vec![sidecar_ip.into()])
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        for attempt in 0..=FIND_RESPONSE_STALL_THRESHOLD {
            let request = if attempt % 2 == 0 {
                Request::FindBlocks {
                    known_blocks: vec![],
                    stop: None,
                }
            } else {
                Request::FindHeaders {
                    known_blocks: vec![],
                    stop: None,
                }
            };
            assert!(respond_to_request(
                &mut peer_set,
                &mut sidecar_handle,
                request,
                Err(PeerError::ConnectionReceiveTimeout.into()),
            )
            .await
            .is_err());
        }

        // If sidecar responses were tracked, this poll would process the final
        // stall event and disconnect it.
        let _ = peer_set.ready().now_or_never();

        assert!(
            sidecar_handle.wants_connection_heartbeats(),
            "zcashd-compat sidecar should not be disconnected by the sync stall detector"
        );
    });
}

/// Configured sidecars receive sampled block adverts without waiting for ordinary responses.
#[test]
fn busy_sidecar_receives_queued_block_gossip() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block_hash = block::Hash::from(&block);

    let sidecar_ip = Ipv4Addr::LOCALHOST;
    let sidecar_addr: PeerSocketAddr = SocketAddr::new(IpAddr::V4(sidecar_ip), 1).into();
    let (sidecar, mut sidecar_handle) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_connected_addr(ConnectedAddr::new_inbound_direct(sidecar_addr))
        .finish();
    let discovered_peers = stream::iter([Ok::<_, BoxError>(Change::Insert(
        sidecar_addr,
        sidecar.into(),
    ))])
    .chain(stream::pending());
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_block_gossip_peer_ips(vec![sidecar_ip.into()])
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        // Make the sidecar busy with an in-flight request.
        let peer_ready = peer_set.ready().await.expect("peer set is ready");
        let find_blocks_fut = peer_ready.call(Request::FindBlocks {
            known_blocks: vec![],
            stop: None,
        });
        let find_blocks_request = sidecar_handle
            .try_to_receive_outbound_client_request()
            .request()
            .expect("sidecar received the find blocks request");

        let mut advertiser = peer_set.block_advertiser();
        let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
        let advert_response = advertiser
            .ready()
            .await
            .unwrap()
            .call(Request::AdvertiseBlock(block_hash, None));
        assert_eq!(sidecar_handle.receive_block_advert(), block_hash);
        advert_response.await.unwrap();

        // Complete the in-flight request, making the sidecar ready again.
        let _ = find_blocks_request
            .tx
            .send(Ok(Response::BlockHashes(vec![])));
        find_blocks_fut.await.expect("response received");
    });
}

/// Returns mock peers with `services`, as a discovery stream, their addresses, and their handles.
///
/// All peers use the same IP, so the peer set needs a `max_conns_per_ip` of at least
/// `services.len()`.
fn mock_peers_with_services(
    services: &[PeerServices],
) -> (
    impl Stream<Item = Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> + Unpin,
    Vec<PeerSocketAddr>,
    Vec<ClientTestHarness>,
) {
    let mut changes = Vec::new();
    let mut addrs = Vec::new();
    let mut handles = Vec::new();

    for (port, services) in (1..).zip(services) {
        let addr: PeerSocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port).into();
        let (client, handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_services(*services)
            .finish();

        changes.push(Ok(Change::Insert(addr, client.into())));
        addrs.push(addr);
        handles.push(handle);
    }

    (
        stream::iter(changes).chain(stream::pending()),
        addrs,
        handles,
    )
}

/// The services of a peer that serves historic blocks, then a peer that doesn't.
const SERVING_AND_NON_SERVING: [PeerServices; 2] =
    [PeerServices::NODE_NETWORK, PeerServices::empty()];

/// Returns a peer set with mock peers that advertised `services`, their addresses and handles, and
/// the chain tip sender for the peer set.
///
/// Must be called from inside a Tokio runtime.
#[allow(clippy::type_complexity)]
fn peer_set_with_services(
    services: &[PeerServices],
) -> (
    PeerSet<
        impl Stream<Item = Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> + Unpin,
        MockChainTip,
    >,
    PeerSetGuard,
    Vec<PeerSocketAddr>,
    Vec<ClientTestHarness>,
    MockChainTipSender,
) {
    let (discovered_peers, addrs, handles) = mock_peers_with_services(services);
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // All mock peers use the same IP.
    let (peer_set, peer_set_guard) = PeerSetBuilder::new()
        .with_discover(discovered_peers)
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(max(services.len(), DEFAULT_MAX_CONNS_PER_IP))
        .build();

    (peer_set, peer_set_guard, addrs, handles, best_tip)
}

/// Sends an inventory change to the peer set.
///
/// The test inventory channel only holds one change, so poll the peer set before sending the next
/// one.
fn send_inventory(peer_set_guard: &mut PeerSetGuard, change: InventoryChange) {
    peer_set_guard
        .inventory_sender()
        .as_mut()
        .expect("unexpected missing inv sender")
        .send(change)
        .expect("unexpected dropped receiver");
}

/// Returns a single block request for a test block hash made from `byte`.
fn block_request(byte: u8) -> Request {
    Request::BlocksByHash(iter::once(block::Hash([byte; 32])).collect())
}

/// Both discovery requests and their corresponding nonempty responses.
fn find_requests_and_responses() -> [(Request, Response); 2] {
    let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .expect("test block is valid");

    [
        (
            Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            Response::BlockHashes(vec![block::Hash::from(&block)]),
        ),
        (
            Request::FindHeaders {
                known_blocks: vec![],
                stop: None,
            },
            Response::BlockHeaders(vec![block::CountedHeader {
                header: block.header,
            }]),
        ),
    ]
}

/// Makes the first mock peer in the peer set busy, by queuing 2 block requests for it.
///
/// Block requests prefer serving peers, so the first peer must be the only serving peer.
/// The mock peer channel holds 2 requests, so the peer only becomes busy after the second one.
async fn make_serving_peer_busy<D, C>(
    peer_set: &mut PeerSet<D, C>,
    serving_addr: PeerSocketAddr,
) -> Vec<<PeerSet<D, C> as Service<Request>>::Future>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    let mut futs = Vec::new();
    for byte in [1, 9] {
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        futs.push(peer_ready.call(block_request(byte)));
    }

    peer_set
        .ready()
        .await
        .expect("peer set service is always ready");
    assert!(
        peer_set.cancel_handles.contains_key(&serving_addr),
        "serving peer should be busy after 2 queued requests",
    );

    futs
}

/// Returns the request a mock peer received, if any.
fn received_request(handle: &mut ClientTestHarness) -> Option<Request> {
    handle
        .try_to_receive_outbound_client_request()
        .request()
        .map(|ClientRequest { request, .. }| request)
}

/// Asserts that `response` is a synthetic `NotFoundRegistry` error.
fn assert_not_found_registry(response: Result<Response, BoxError>) {
    let error = response.expect_err("peer set should refuse the request");
    let error = error
        .downcast_ref::<SharedPeerError>()
        .expect("peer set should return a boxed SharedPeerError");
    assert!(
        error.inner_debug().contains("NotFoundRegistry"),
        "unexpected error: {error:?}"
    );
}

/// Discovery must not rotate onto a non-serving peer while a serving peer is ready.
#[test]
fn peer_set_routes_find_requests_to_ready_serving_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        for (request, response) in find_requests_and_responses() {
            assert_eq!(
                respond_to_request(
                    &mut peer_set,
                    &mut handles[0],
                    request,
                    Ok(response.clone()),
                )
                .await
                .expect("the serving peer answers discovery"),
                response,
            );
            assert_eq!(received_request(&mut handles[1]), None);
        }
    });
}

/// A busy serving peer must get queued discovery before a ready non-serving peer does.
#[test]
fn peer_set_routes_queued_find_request_to_serving_peer_once_ready() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let mut queued_fut = peer_set
                .ready()
                .await
                .expect("the non-serving peer keeps the peer set ready")
                .call(request.clone());

            assert_eq!(received_request(&mut handles[1]), None);
            assert!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4, &mut queued_fut)
                    .await
                    .is_err(),
                "discovery must wait rather than fail while the serving peer is busy",
            );
            assert_eq!(received_request(&mut handles[1]), None);

            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
            peer_set
                .ready()
                .await
                .expect("the serving peer is ready again");

            let client_request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("the serving peer receives queued discovery as soon as it is ready");
            assert_eq!(client_request.request, request);
            assert_eq!(received_request(&mut handles[1]), None);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, queued_fut)
                    .await
                    .expect("queued discovery must not hang")
                    .expect("the serving peer answers discovery"),
                response,
            );
        }
    });
}

/// Readiness wakes queued discovery behind `Buffer` without another user request.
#[test]
fn peer_set_routes_queued_find_request_behind_buffer_without_other_requests() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let (mut peer_set, _poll_task) = peer_set.into_buffer(10);
            let mut queued_fut = peer_set
                .ready()
                .await
                .expect("the buffer has room")
                .call(request.clone());

            assert!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4, &mut queued_fut)
                    .await
                    .is_err(),
                "buffered discovery must wait while the serving peer is busy",
            );
            assert_eq!(received_request(&mut handles[1]), None);
            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));

            // No more calls or readiness polls: only the peer's wakeup can route discovery.
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4).await;
            let client_request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("the peer readiness wakeup routes buffered discovery");
            assert_eq!(client_request.request, request);
            assert_eq!(received_request(&mut handles[1]), None);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, queued_fut)
                    .await
                    .expect("buffered discovery must not hang")
                    .expect("the serving peer answers discovery"),
                response,
            );
        }
    });
}

/// With no serving peers, discovery is sent immediately rather than waiting for the deadline.
#[test]
fn peer_set_routes_find_request_immediately_without_serving_peers() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&[PeerServices::empty()]);

        for (request, response) in find_requests_and_responses() {
            assert_eq!(
                respond_to_request(
                    &mut peer_set,
                    &mut handles[0],
                    request,
                    Ok(response.clone()),
                )
                .await
                .expect("a non-serving peer can answer discovery"),
                response,
            );
        }
    });
}

/// A queued block must not keep later discovery in `Buffer` or take its newly ready peer.
#[test]
fn peer_set_routes_queued_find_before_earlier_block_behind_buffer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let (mut peer_set, _poll_task) = peer_set.into_buffer(10);
            let block_fut = peer_set
                .ready()
                .await
                .expect("the buffer has room")
                .call(block_request(2));
            let find_fut = peer_set
                .ready()
                .await
                .expect("the buffer has room for discovery after the block")
                .call(request.clone());

            // Let both requests reach PeerSet while the serving peer remains busy.
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4).await;
            assert_eq!(received_request(&mut handles[1]), None);
            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4).await;

            let client_request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("discovery gets the newly ready serving peer before the earlier block");
            assert_eq!(client_request.request, request);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, find_fut)
                    .await
                    .expect("prioritized discovery must not hang")
                    .expect("the serving peer answers discovery"),
                response,
            );

            // The earlier block is still routed, after discovery.
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4).await;
            let client_request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("the earlier block remains queued behind discovery");
            assert_eq!(client_request.request, block_request(2));
            client_request
                .tx
                .send(Ok(Response::Nil))
                .expect("the earlier block is still pending");
            assert_eq!(
                timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, block_fut)
                    .await
                    .expect("the earlier block must not hang")
                    .expect("the serving peer answers the earlier block"),
                Response::Nil,
            );
            assert_eq!(received_request(&mut handles[1]), None);
        }
    });
}

/// A request held in the `Buffer` must not wait until every queued block request has been routed,
/// even when every peer that becomes ready is a serving peer that can take those blocks.
#[test]
fn peer_set_gives_buffered_request_a_turn_behind_queued_blocks() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        let find = find_requests_and_responses()[0].0.clone();
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&[PeerServices::NODE_NETWORK; 2]);

        // Only the first serving peer can serve these blocks, but the second one keeps the peer
        // set ready, so the blocks queue behind the first one.
        peer_set.ready().await.expect("peers connect");
        let missing: Vec<InventoryHash> = [1, 9, 2, 3, 4]
            .into_iter()
            .map(|byte| InventoryHash::Block(block::Hash([byte; 32])))
            .collect();
        send_inventory(
            &mut peer_set_guard,
            InventoryChange::new_missing_multi(missing.iter(), addrs[1])
                .expect("hashes are not empty"),
        );
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
        let mut queued_futs = Vec::new();
        for byte in [2, 3, 4] {
            queued_futs.push(
                peer_set
                    .ready()
                    .await
                    .expect("the second serving peer keeps the peer set ready")
                    .call(block_request(byte)),
            );
        }
        // Make the second serving peer busy too, so the peer set has no ready peers.
        let mut other_futs = Vec::new();
        for _ in 0..2 {
            other_futs.push(
                peer_set
                    .ready()
                    .await
                    .expect("the second serving peer is ready")
                    .call(Request::Peers),
            );
        }

        let (mut peer_set, _poll_task) = peer_set.into_buffer(10);
        let _find_fut = peer_set
            .ready()
            .await
            .expect("the buffer has room")
            .call(find.clone());
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Free one slot on the first peer: the first queued block takes it from the held request.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(2)));

        // The held discovery request gets the next ready peer, before the rest of the queue.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            received_request(&mut handles[0]),
            Some(find),
            "the request held by the buffer must not wait for every queued block",
        );
        assert_eq!(received_request(&mut handles[0]), Some(block_request(3)));

        // The rest of the queue is still routed afterwards.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(received_request(&mut handles[0]), Some(block_request(4)));
        assert_eq!(received_request(&mut handles[1]), Some(Request::Peers));
        assert_eq!(received_request(&mut handles[1]), Some(Request::Peers));
        assert_eq!(received_request(&mut handles[1]), None);
    });
}

/// A ready peer kept for the request held in the `Buffer` goes to queued block requests if that
/// request doesn't use it. Block adverts no longer queue in the peer set, so they can't take it.
#[test]
fn peer_set_routes_unused_kept_peer_to_queued_blocks() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&[PeerServices::NODE_NETWORK; 2]);

        // Only the first serving peer can serve these blocks, but the second one keeps the peer
        // set ready, so the last block queues behind the first one.
        peer_set.ready().await.expect("peers connect");
        let missing: Vec<InventoryHash> = [1, 9, 2]
            .into_iter()
            .map(|byte| InventoryHash::Block(block::Hash([byte; 32])))
            .collect();
        send_inventory(
            &mut peer_set_guard,
            InventoryChange::new_missing_multi(missing.iter(), addrs[1])
                .expect("hashes are not empty"),
        );
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
        let _queued_fut = peer_set
            .ready()
            .await
            .expect("the second serving peer keeps the peer set ready")
            .call(block_request(2));
        // Make the second serving peer busy too, so the peer set has no ready peers.
        let mut other_futs = Vec::new();
        for _ in 0..2 {
            other_futs.push(
                peer_set
                    .ready()
                    .await
                    .expect("the second serving peer is ready")
                    .call(Request::Peers),
            );
        }

        // Queued block requests already took the last ready peer from the held request.
        peer_set.queued_inv_requests_took_last_peer = true;

        // Free one slot on the first peer. It is kept for the held request. If that request
        // doesn't use it, like a poll from the queued request task, the queued block gets it on
        // the next poll.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert!(
            futures::poll!(peer_set.ready()).is_ready(),
            "the peer kept for the held request makes the peer set ready",
        );
        assert_eq!(peer_set.queued_inv_requests.len(), 1);
        let waker = peer_set.maintenance_waker.clone();
        let _ = futures::poll!(std::pin::pin!(waker.notify.notified()));
        let _poll_fut = peer_set.call(Request::PollPeerSet);
        assert!(
            futures::poll!(std::pin::pin!(waker.notify.notified())).is_ready(),
            "the queued request task must be woken to route the unused peer",
        );
        let _ = futures::poll!(peer_set.ready());
        // The first peer still has the request it was busy with before the queued block.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        assert_eq!(
            received_request(&mut handles[0]),
            Some(block_request(2)),
            "the queued block takes the peer the poll left unused",
        );
    });
}

/// The deadline starts when discovery is queued, even if its response future is never polled.
#[test]
fn peer_set_routes_queued_find_to_non_serving_peer_after_wait_timeout() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let queued_fut = peer_set
                .ready()
                .await
                .expect("the non-serving peer keeps the peer set ready")
                .call(request.clone());

            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 2).await;
            peer_set
                .ready()
                .await
                .expect("the non-serving peer is ready");
            assert_eq!(received_request(&mut handles[1]), None);
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 2 + Duration::from_millis(1))
                .await;
            peer_set
                .ready()
                .await
                .expect("the non-serving peer is ready");

            let client_request = handles[1]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("discovery falls back to a ready non-serving peer after its deadline");
            assert_eq!(client_request.request, request);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, queued_fut)
                    .await
                    .expect("fallback discovery must not hang")
                    .expect("the fallback peer answers discovery"),
                response,
            );

            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
            peer_set
                .ready()
                .await
                .expect("the serving peer is ready again");
            assert_eq!(received_request(&mut handles[0]), None);
            assert_eq!(received_request(&mut handles[1]), None);
        }
    });
}

/// Expiry alone wakes queued discovery behind `Buffer`, without polling the response future.
#[test]
fn peer_set_routes_queued_find_after_timeout_behind_buffer_without_other_requests() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let (mut peer_set, _poll_task) = peer_set.into_buffer(10);
            let queued_fut = peer_set
                .ready()
                .await
                .expect("the buffer has room")
                .call(request.clone());

            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 2).await;
            assert_eq!(received_request(&mut handles[1]), None);
            // Neither the busy peer nor any new user request can wake the peer set.
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 2 + Duration::from_millis(1))
                .await;
            let client_request = handles[1]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("the timer wakeup routes buffered discovery to the fallback peer");
            assert_eq!(client_request.request, request);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, queued_fut)
                    .await
                    .expect("buffered fallback discovery must not hang")
                    .expect("the fallback peer answers discovery"),
                response,
            );
        }
    });
}

/// Dropping an unpolled queued response must prevent routing on readiness and later expiry.
#[test]
fn peer_set_does_not_route_cancelled_queued_find_request() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, _) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let queued_fut = peer_set
                .ready()
                .await
                .expect("the non-serving peer keeps the peer set ready")
                .call(request);
            drop(queued_fut);

            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
            peer_set
                .ready()
                .await
                .expect("the serving peer is ready again");
            assert_eq!(received_request(&mut handles[0]), None);
            assert_eq!(received_request(&mut handles[1]), None);

            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT + Duration::from_millis(1))
                .await;
            peer_set.ready().await.expect("both peers are still ready");
            assert_eq!(received_request(&mut handles[0]), None);
            assert_eq!(received_request(&mut handles[1]), None);
            assert!(handles
                .iter_mut()
                .all(|handle| handle.wants_connection_heartbeats()));
        }
    });
}

/// Losing the last serving peer allows immediate fallback, without waiting for expiry.
#[test]
fn peer_set_routes_queued_find_after_serving_peer_disconnects() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async move {
        for (request, response) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let (mut peer_set, _poll_task) = peer_set.into_buffer(10);
            let mut queued_fut = peer_set
                .ready()
                .await
                .expect("the buffer has room")
                .call(request.clone());
            assert!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4, &mut queued_fut)
                    .await
                    .is_err(),
                "discovery waits while the serving peer is connected",
            );
            assert_eq!(received_request(&mut handles[1]), None);

            handles[0].close_outbound_client_request_receiver();
            tokio::time::sleep(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT / 4).await;
            let client_request = handles[1]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("losing the serving peer wakes queued discovery before its deadline");
            assert_eq!(client_request.request, request);
            client_request
                .tx
                .send(Ok(response.clone()))
                .expect("queued discovery is still pending");
            assert_eq!(
                timeout(FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, queued_fut)
                    .await
                    .expect("discovery after disconnection must not hang")
                    .expect("the fallback peer answers discovery"),
                response,
            );
        }
    });
}

/// Check that block requests that no peer advertised go to a peer that can serve historic blocks,
/// rather than a non-serving peer that is likely to answer `notfound`.
#[test]
fn peer_set_route_block_prefers_serving_peer() {
    peer_set_route_block_prefers_serving_peer_order(true);
    peer_set_route_block_prefers_serving_peer_order(false);
}

fn peer_set_route_block_prefers_serving_peer_order(serving_first: bool) {
    let mut services = SERVING_AND_NON_SERVING;
    if !serving_first {
        services.reverse();
    }
    let (serving, non_serving) = if serving_first { (0, 1) } else { (1, 0) };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&services);

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(peer_ready.ready_services.len(), 2);

        let _fut = peer_ready.call(block_request(1));

        assert_eq!(
            received_request(&mut handles[serving]),
            Some(block_request(1)),
            "block request should be routed to the serving peer",
        );
        assert_eq!(
            received_request(&mut handles[non_serving]),
            None,
            "block request should not be routed to the non-serving peer",
        );
    });
}

/// Check that when the only block-serving peer is busy, block requests wait for it, instead of
/// going to a ready non-serving peer, or being refused instantly. The waiting request is routed to
/// the serving peer as soon as it is ready again, without a retry.
///
/// This is the mainnet stall from Zebra 6.4.1: while all serving peers were busy, the syncer used
/// up its retries on instant local refusals, and dropped the block after the chain tip.
#[test]
fn peer_set_routes_queued_block_request_to_serving_peer_once_ready() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        // Make the serving peer busy: its request stays queued until the test receives it.
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        let peer_ready = peer_set.ready().await.expect("peer set service is always ready");
        assert_eq!(
            peer_ready.ready_services.len(),
            1,
            "only the non-serving peer should be ready"
        );

        let mut queued_fut = peer_ready.call(block_request(2));

        assert_eq!(
            received_request(&mut handles[1]),
            None,
            "block request should not be routed to the non-serving peer while a serving peer is busy",
        );
        assert!(
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, &mut queued_fut)
                .await
                .is_err(),
            "block request should wait while a serving peer is busy",
        );

        // Let the serving peer take its queued requests, so it becomes ready again.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));

        // Polling the peer set routes the waiting request to the newly ready serving peer.
        peer_set.ready().await.expect("peer set service is always ready");

        let ClientRequest { request, tx, .. } = handles[0]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("the waiting block request should be routed to the now-ready serving peer");
        assert_eq!(request, block_request(2));
        assert_eq!(received_request(&mut handles[1]), None);

        let _ = tx.send(Ok(Response::Nil));
        let response = timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, queued_fut)
            .await
            .expect("the waiting request should resolve to the serving peer's response");
        assert!(
            matches!(response, Ok(Response::Nil)),
            "unexpected response: {response:?}"
        );
    });
}

/// Check that consecutive queued block requests reach the busy serving peer when it becomes
/// ready, without unrelated requests waking the peer set.
///
/// In zebrad, the peer set is behind a `Buffer`, which only polls it when it has a request. So the
/// task spawned by [`PeerSet::into_buffer`] has to poll it when the busy peer becomes ready.
#[test]
fn peer_set_routes_queued_block_request_behind_buffer_without_other_requests() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        let (mut peer_set, _poll_task) = peer_set.into_buffer(10);

        // Fill the serving peer's 2-slot channel, then queue two more block requests.
        let mut futs = Vec::new();
        for byte in [1, 9, 2, 3] {
            let peer_ready = peer_set
                .ready()
                .await
                .expect("peer set service is always ready");
            futs.push(peer_ready.call(block_request(byte)));
        }
        let second_queued_fut = futs.pop().expect("just pushed the second queued request");
        let first_queued_fut = futs.pop().expect("just pushed the first queued request");

        // Let the buffer route the requests.
        tokio::time::sleep(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4).await;
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        assert_eq!(
            received_request(&mut handles[1]),
            None,
            "block request should not be routed to the non-serving peer while a serving peer is busy",
        );

        // Receiving the serving peer's requests makes it ready. The test doesn't send any other
        // requests to the peer set, so only the poll task can route the queued request.
        tokio::time::sleep(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4).await;

        for (byte, queued_fut) in [(2, first_queued_fut), (3, second_queued_fut)] {
            let ClientRequest { request, tx, .. } = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("each queued request should reach the now-ready serving peer");
            assert_eq!(request, block_request(byte));
            assert_eq!(received_request(&mut handles[1]), None);

            let _ = tx.send(Ok(Response::Nil));
            let response = timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, queued_fut)
                .await
                .expect("the queued request should resolve to the serving peer's response");
            assert!(
                matches!(response, Ok(Response::Nil)),
                "unexpected response: {response:?}"
            );
        }
    });
}

/// Readiness expiry must revoke the separate advert path before maintenance is acknowledged.
#[tokio::test(start_paused = true)]
async fn readiness_expiry_removes_busy_peer_from_advertisers() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&SERVING_AND_NON_SERVING);
    let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
    assert!(peer_set
        .block_advert_senders
        .borrow()
        .contains_key(&addrs[0]));

    tokio::time::advance(REQUEST_TIMEOUT).await;
    timeout(Duration::from_secs(1), peer_set.ready())
        .await
        .expect("the non-serving peer remains ready")
        .expect("readiness expiry is not a peer-set error");

    let mut advertiser = peer_set.block_advertiser();
    let (_network, _poll_task) = peer_set.into_buffer(PEERSET_BUFFER_SIZE);
    timeout(Duration::from_secs(1), async {
        let hash = block::Hash([14; 32]);
        let advert = advertiser
            .ready()
            .await
            .expect("the surviving peer remains eligible for adverts")
            .call(Request::AdvertiseBlockToAll(hash));
        assert_eq!(handles[1].receive_block_advert(), hash);
        assert_eq!(
            advert
                .await
                .expect("the surviving peer acknowledged the advert"),
            Response::Nil
        );
        assert!(
            handles[0].block_advert_receiver.try_recv().is_err(),
            "the expired peer must not receive the advert"
        );
    })
    .await
    .expect("advertising to the surviving peer does not wait for the expired peer");
}

/// Check that a waiting block request is refused after [`INVENTORY_BUSY_PEER_WAIT_TIMEOUT`] if the
/// busy serving peer doesn't become ready, and that it isn't routed to that peer afterwards.
#[test]
fn peer_set_refuses_queued_block_request_after_wait_timeout() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let mut queued_fut = peer_ready.call(block_request(2));

        assert!(
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 2, &mut queued_fut)
                .await
                .is_err(),
            "refusal should wait while a serving peer is busy",
        );
        assert_not_found_registry(
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT, queued_fut)
                .await
                .expect("queued refusal must complete within its bounded wait"),
        );

        // Once the serving peer is ready again, the refused request isn't routed to it.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        assert_eq!(received_request(&mut handles[0]), None);
        assert_eq!(received_request(&mut handles[1]), None);
    });
}

/// Queue expiration starts at admission, even if the response future has never been polled.
#[test]
fn peer_set_expires_unpolled_queued_block_requests() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        for poll_before_ready in [true, false] {
            let (mut peers, _peer_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy = make_serving_peer_busy(&mut peers, addrs[0]).await;
            let mut queued = peers.ready().await.unwrap().call(block_request(2));
            assert_eq!(peers.queued_inv_requests.len(), 1);
            tokio::time::advance(INVENTORY_BUSY_PEER_WAIT_TIMEOUT + Duration::from_millis(1)).await;

            if poll_before_ready {
                assert_not_found_registry(
                    timeout(Duration::from_millis(1), &mut queued)
                        .await
                        .expect("a delayed first poll must not start another queue wait"),
                );
            }
            assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
            peers.ready().await.unwrap();
            assert!(peers.queued_inv_requests.is_empty());
            assert_eq!(received_request(&mut handles[0]), None);
            assert_eq!(received_request(&mut handles[1]), None);
            if !poll_before_ready {
                assert_not_found_registry(
                    queued
                        .now_or_never()
                        .expect("dispatch must refuse an expired entry before its first poll"),
                );
            }
        }
    });
}

/// Check that recent blocks, which peers have advertised, still go to a ready non-serving peer
/// while the only serving peer is busy. Non-serving peers can usually serve recent blocks, and
/// the inbound block gossip downloader doesn't retry refused requests.
#[test]
fn peer_set_routes_advertised_block_to_non_serving_peer_while_serving_peer_busy() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // The busy serving peer advertised the block, so it is a recent block.
        let advertised_inv = InventoryHash::Block(block::Hash([2; 32]));
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_available(advertised_inv, addrs[0]),
        );

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let _fut = peer_ready.call(block_request(2));

        assert_eq!(
            received_request(&mut handles[1]),
            Some(block_request(2)),
            "an advertised block should be routed to the ready non-serving peer",
        );
    });
}

/// Check that block requests still go to non-serving peers if no serving peer is connected.
#[test]
fn peer_set_routes_block_to_non_serving_peer_without_serving_peers() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&[PeerServices::empty(), PeerServices::empty()]);

        // The mock peer channels hold 2 requests each, so after 3 requests one peer is busy. Then
        // check the other peer still gets the 4th request immediately.
        let mut futs = Vec::new();
        for byte in 1..=4 {
            let peer_ready = peer_set.ready().await.expect("peer set service is always ready");
            futs.push(peer_ready.call(block_request(byte)));
        }

        let received = handles
            .iter_mut()
            .map(|handle| iter::from_fn(|| received_request(handle)).count())
            .sum::<usize>();

        assert_eq!(
            received, 4,
            "block requests should still be routed to non-serving peers if no serving peer is connected",
        );
    });
}

/// Check that block requests only wait for a busy non-serving peer if the block was advertised.
///
/// Non-serving peers usually can't serve historic blocks, so waiting for one to finish its current
/// request would only slow down the refusal. But peers advertise recent blocks, which non-serving
/// peers can usually serve.
#[test]
fn peer_set_only_waits_for_busy_non_serving_peer_if_block_advertised() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, _handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);
        let (serving_addr, non_serving_addr) = (addrs[0], addrs[1]);

        // Make the non-serving peer busy, by marking the serving peer as missing the requested
        // blocks. The mock peer channel holds 2 requests.
        let mut busy_futs = Vec::new();
        for byte in [1, 9] {
            send_inventory(
                &mut peer_set_guard,
                InventoryStatus::new_missing(
                    InventoryHash::Block(block::Hash([byte; 32])),
                    serving_addr,
                ),
            );
            let peer_ready = peer_set
                .ready()
                .await
                .expect("peer set service is always ready");
            busy_futs.push(peer_ready.call(block_request(byte)));
        }

        // A historic block that the ready serving peer is missing: the busy non-serving peer
        // can't serve it either, so the refusal is instant.
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_missing(InventoryHash::Block(block::Hash([2; 32])), serving_addr),
        );
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert!(
            peer_ready.cancel_handles.contains_key(&non_serving_addr),
            "non-serving peer should be busy after 2 queued requests",
        );

        let refused_fut = peer_ready.call(block_request(2));
        let response = timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 2, refused_fut)
            .await
            .expect("a historic block request should not wait for a busy non-serving peer");
        assert_not_found_registry(response);

        // A recent block that the busy non-serving peer advertised: the request waits for that
        // peer to become ready.
        let advertised_hash = block::Hash([3; 32]);
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_missing(InventoryHash::Block(advertised_hash), serving_addr),
        );
        peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_available(InventoryHash::Block(advertised_hash), non_serving_addr),
        );
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        let mut queued_fut = peer_ready.call(block_request(3));
        assert!(
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 2, &mut queued_fut)
                .await
                .is_err(),
            "a block request should wait for a busy non-serving peer that advertised it",
        );
        assert_not_found_registry(queued_fut.await);
    });
}

/// Check that block requests are refused instantly if every connected peer, ready or busy, is
/// missing the block.
#[test]
fn peer_set_refuses_block_instantly_if_all_peers_missing() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        // Make the serving peer busy.
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // Mark the requested block as missing on both peers. The test inventory channel only holds
        // one change, so poll the peer set to process each change before sending the next one.
        let missing_inv = InventoryHash::Block(block::Hash([2; 32]));
        for addr in addrs {
            send_inventory(
                &mut peer_set_guard,
                InventoryStatus::new_missing(missing_inv, addr),
            );
            peer_set
                .ready()
                .await
                .expect("peer set service is always ready");
        }

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let refused_fut = peer_ready.call(block_request(2));

        let response = timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 2, refused_fut)
            .await
            .expect("refusal should be instant when every peer is missing the block");
        assert_not_found_registry(response);

        assert_eq!(received_request(&mut handles[1]), None);
    });
}

/// Check that transaction requests are still refused instantly while a serving peer is busy:
/// non-serving peers can serve mempool transactions, and transaction downloads don't retry.
#[test]
fn peer_set_refuses_transaction_instantly_while_serving_peer_busy() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        // Make the serving peer busy.
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // Mark the requested transaction as missing on the ready non-serving peer only.
        let tx_id = UnminedTxId::Legacy(transaction::Hash([3; 32]));
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_missing(InventoryHash::from(tx_id), addrs[1]),
        );

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let refused_fut = peer_ready.call(Request::TransactionsById(iter::once(tx_id).collect()));

        let response = timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 2, refused_fut)
            .await
            .expect("transaction refusals should not be delayed");
        assert_not_found_registry(response);

        assert_eq!(received_request(&mut handles[1]), None);
    });
}

/// Check that an advertisement from a disconnected peer doesn't make a historic block go to a
/// ready non-serving peer while a serving peer is busy.
#[test]
fn peer_set_ignores_disconnected_advertisers() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // A peer that isn't connected advertised the block.
        let disconnected_addr: PeerSocketAddr =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 1).into();
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_available(
                InventoryHash::Block(block::Hash([2; 32])),
                disconnected_addr,
            ),
        );

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let mut fut = peer_ready.call(block_request(2));

        assert_eq!(
            received_request(&mut handles[1]),
            None,
            "a block advertised by a disconnected peer should wait for the busy serving peer",
        );
        assert!(
            futures::poll!(&mut fut).is_pending(),
            "the request waits instead of being refused at once",
        );
    });
}

/// Sends a block request to the only serving peer, then completes it with a response timeout.
async fn fail_serving_peer_block_request<D, C>(
    peer_set: &mut PeerSet<D, C>,
    handles: &mut [ClientTestHarness],
) where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    respond_to_request(
        peer_set,
        &mut handles[0],
        block_request(2),
        Err(PeerError::ConnectionReceiveTimeout.into()),
    )
    .await
    .expect_err("the serving peer should fail the request");
}

/// Check that a block request isn't routed to a serving peer that just failed to return that
/// block, if another peer might have it.
///
/// Peers declare their own services, and request timeouts don't mark inventory as missing. So
/// otherwise a peer that falsely claims to serve blocks, and withholds them, would get every
/// retry for the block.
#[test]
fn peer_set_routes_block_retry_away_from_failed_serving_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        fail_serving_peer_block_request(&mut peer_set, &mut handles).await;

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let _retry_fut = peer_ready.call(block_request(2));

        assert_eq!(
            received_request(&mut handles[0]),
            None,
            "the retry should not be routed to the serving peer that failed it",
        );
        assert_eq!(
            received_request(&mut handles[1]),
            Some(block_request(2)),
            "the retry should be routed to the non-serving peer",
        );

        // Other blocks still prefer the serving peer.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let _other_fut = peer_ready.call(block_request(3));

        assert_eq!(
            received_request(&mut handles[0]),
            Some(block_request(3)),
            "other blocks should still be routed to the serving peer",
        );
    });
}

/// Check that a block request is still routed to a peer that failed to return that block, if no
/// other peer might have it.
#[test]
fn peer_set_retries_block_with_failed_peer_as_last_resort() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard, _addrs, mut handles, _best_tip) =
            peer_set_with_services(&[PeerServices::NODE_NETWORK]);

        fail_serving_peer_block_request(&mut peer_set, &mut handles).await;

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let _retry_fut = peer_ready.call(block_request(2));

        assert_eq!(
            received_request(&mut handles[0]),
            Some(block_request(2)),
            "the retry should be routed to the only peer, even though it failed the block",
        );
    });
}

/// Check that a block request waits for a busy peer that failed to return that block, if no other
/// peer might have it, instead of being refused instantly.
#[test]
fn peer_set_waits_for_busy_failed_peer_as_last_resort() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard, addrs, mut handles, _best_tip) =
            peer_set_with_services(&SERVING_AND_NON_SERVING);

        // The non-serving peer is ready, but it doesn't have the block.
        send_inventory(
            &mut peer_set_guard,
            InventoryStatus::new_missing(InventoryHash::Block(block::Hash([2; 32])), addrs[1]),
        );

        fail_serving_peer_block_request(&mut peer_set, &mut handles).await;
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let mut queued_fut = peer_ready.call(block_request(2));

        assert!(
            timeout(INVENTORY_BUSY_PEER_WAIT_TIMEOUT / 4, &mut queued_fut)
                .await
                .is_err(),
            "the retry should wait for the busy peer, even though it failed the block",
        );

        // Let the peer take its queued requests, so it becomes ready again.
        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        assert_eq!(
            received_request(&mut handles[0]),
            Some(block_request(2)),
            "the waiting retry should be routed to the peer once it is ready",
        );
    });
}
