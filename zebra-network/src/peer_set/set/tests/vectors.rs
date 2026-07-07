//! Fixed test vectors for the peer set.

use std::{cmp::max, collections::HashSet, iter, net::SocketAddr, sync::Arc, time::Duration};

use futures::FutureExt as _;
use tokio::time::timeout;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block,
    parameters::{Network, NetworkUpgrade},
};

use crate::{
    constants::DEFAULT_MAX_CONNS_PER_IP,
    peer::{ClientRequest, MinimumPeerVersion},
    peer_set::{inventory_registry::InventoryStatus, stall_tracker::FIND_RESPONSE_STALL_THRESHOLD},
    protocol::external::{types::Version, InventoryHash},
    PeerSocketAddr, Request, Response, SharedPeerError,
};
use indexmap::IndexMap;
use tokio::sync::watch;

use super::{PeerSetBuilder, PeerVersions};

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

#[test]
fn broadcast_all_queued_removes_banned_peers() {
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

        let banned_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let mut bans_map: IndexMap<std::net::IpAddr, std::time::Instant> = IndexMap::new();
        bans_map.insert(banned_ip, std::time::Instant::now());

        let (bans_tx, bans_rx) = watch::channel(Arc::new(bans_map));
        let _ = bans_tx;
        peer_set.bans_receiver = bans_rx;

        let banned_addr: PeerSocketAddr = SocketAddr::new(banned_ip, 1).into();
        let mut remaining_peers = HashSet::new();
        remaining_peers.insert(banned_addr);

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        peer_set.queued_broadcast_all = Some((Request::Peers, sender, remaining_peers));

        peer_set.broadcast_all_queued();

        if let Some((_req, _sender, remaining_peers)) = peer_set.queued_broadcast_all.take() {
            assert!(remaining_peers.is_empty());
        } else {
            assert!(receiver.try_recv().is_ok());
        }
    });
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
        let mut bans_map: IndexMap<std::net::IpAddr, std::time::Instant> = IndexMap::new();
        bans_map.insert(banned_ip, std::time::Instant::now());
        let (_bans_tx, bans_rx) = watch::channel(Arc::new(bans_map));
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

/// Check that empty `FindBlocks` responses do not trigger stall tracking when the node is at the
/// chain tip, so peers that correctly return no hashes are not disconnected.
#[test]
fn find_blocks_stall_not_tracked_when_at_tip() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version],
    };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Simulate being at the chain tip.
    best_tip.send_best_tip_height(Some(block::Height(2_500_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(0));

    let mut handle = handles.into_iter().next().expect("there is one peer");

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        // Send more FindBlocks requests than FIND_RESPONSE_STALL_THRESHOLD, each
        // returning an empty response. If stall events were tracked, the peer would be
        // disconnected after the third response.
        let request_count = FIND_RESPONSE_STALL_THRESHOLD + 1;

        for _ in 0..request_count {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            // Reply with an empty BlockHashes response — protocol-correct at tip.
            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        // The peer must still be connected: no stall events were emitted.
        assert!(
            handle.wants_connection_heartbeats(),
            "peer should not be disconnected when at tip"
        );
    });
}

/// Check that empty `FindBlocks` responses DO trigger stall tracking when the node is syncing,
/// and that the peer is disconnected after exceeding the stall threshold.
///
/// This verifies the security property from GHSA-h9hm-m2xj-4rq9 is preserved: peers that
/// return only empty responses during initial sync are still detected and disconnected.
#[test]
fn find_blocks_stall_tracked_when_syncing() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version],
    };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Simulate being far behind the chain tip, as during initial sync.
    best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(10_000));

    let mut handle = handles.into_iter().next().expect("there is one peer");

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        // Send exactly FIND_RESPONSE_STALL_THRESHOLD empty FindBlocks responses.
        // Each response emits a stall event; the third one triggers disconnect.
        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        // One extra poll_ready to drain the final stall event and process the disconnect.
        // Since there are no remaining ready peers, the future does not resolve.
        let _ = peer_set.ready().now_or_never();

        // The peer must be disconnected: stall threshold was reached while syncing.
        assert!(
            !handle.wants_connection_heartbeats(),
            "peer should be disconnected after stall threshold is reached while syncing"
        );
    });
}

/// Check that stall tracking is active when the chain tip state is unknown (empty node state),
/// so that stalling peers are still disconnected even before the first block is synced.
#[test]
fn find_blocks_stall_tracked_when_tip_unknown() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version],
    };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Leave the chain tip in its default state (None height, None distance).
    // is_at_or_near_network_tip returns false when the tip is unknown, so stall
    // tracking is active.

    let mut handle = handles.into_iter().next().expect("there is one peer");

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        let _ = peer_set.ready().now_or_never();

        assert!(
            !handle.wants_connection_heartbeats(),
            "peer should be disconnected when tip is unknown and stall threshold is reached"
        );
    });
}

/// Check that stall counts accumulated while syncing are preserved across a tip transition,
/// so a peer cannot avoid detection by temporarily becoming useful as the node reaches the tip.
///
/// This verifies that returning an empty response at tip does not reset a peer's accumulated
/// stall count. When the node falls back behind tip, one more empty response reaches the
/// threshold and the peer is disconnected.
#[test]
fn find_blocks_stall_count_preserved_across_tip_transition() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version],
    };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (discovered_peers, handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Start syncing: FIND_RESPONSE_STALL_THRESHOLD - 1 stalls away from disconnect.
    best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(10_000));

    let mut handle = handles.into_iter().next().expect("there is one peer");

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        // Accumulate THRESHOLD - 1 stalls while syncing.
        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD - 1 {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        // Transition to at-tip: stall count is now THRESHOLD - 1 (one below disconnect).
        best_tip.send_best_tip_height(Some(block::Height(2_500_000)));
        best_tip.send_estimated_distance_to_network_chain_tip(Some(0));

        // Send one empty response at tip. Since track_stalls is false, no stall event is
        // emitted and the peer's accumulated count is unchanged.
        {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        // Transition back to syncing: count is still THRESHOLD - 1.
        best_tip.send_estimated_distance_to_network_chain_tip(Some(10_000));

        // One more syncing response reaches the threshold.
        {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");

            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });

            let client_request = handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("peer received the request");

            let _ = client_request.tx.send(Ok(Response::BlockHashes(vec![])));

            response_fut.await.expect("response received");
        }

        // One final poll_ready to drain the last stall event and process the disconnect.
        let _ = peer_set.ready().now_or_never();

        // The peer must be disconnected: the accumulated stall count was not reset at tip.
        assert!(
            !handle.wants_connection_heartbeats(),
            "peer should be disconnected: stall count accumulated during sync was preserved"
        );
    });
}
