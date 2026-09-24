//! Fixed test vectors for the peer set.

use std::{
    cmp::max,
    collections::HashSet,
    iter,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use futures::{stream, FutureExt as _, Stream, StreamExt};
use tokio::time::timeout;
use tower::{
    discover::{Change, Discover},
    Service, ServiceExt,
};

use zebra_chain::{
    block,
    chain_tip::{ChainTip, AT_OR_NEAR_TIP_THRESHOLD},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::{self, UnminedTxId},
};

use crate::{
    constants::{CURRENT_NETWORK_PROTOCOL_VERSION, DEFAULT_MAX_CONNS_PER_IP},
    peer::{
        ClientRequest, ClientTestHarness, ConnectedAddr, LoadTrackedClient, MinimumPeerVersion,
    },
    peer_set::{
        inventory_registry::InventoryStatus, inventory_retry::retry_busy_inventory,
        set::InventoryService, stall_tracker::FIND_RESPONSE_STALL_THRESHOLD, PeerSet,
    },
    protocol::external::{
        types::{PeerServices, Version},
        InventoryHash,
    },
    BoxError, PeerError, PeerSocketAddr, Request, Response, SharedPeerError,
};
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
        let mut bans = crate::BanList::default();
        bans.ban(banned_ip);

        let (bans_tx, bans_rx) = watch::channel(bans);
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

    // Simulate being at the maximum estimated distance that is still considered near the tip.
    best_tip.send_best_tip_height(Some(block::Height(2_500_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(AT_OR_NEAR_TIP_THRESHOLD));

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
            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

    // Simulate being just beyond the maximum estimated distance considered near the tip.
    best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(AT_OR_NEAR_TIP_THRESHOLD + 1));

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

            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));

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

/// Check that the sync stall detector does not disconnect the configured zcashd-compat sidecar.
#[test]
fn find_blocks_stall_not_tracked_for_zcashd_compat() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let sidecar_ip = Ipv4Addr::LOCALHOST;
    let sidecar_addr: PeerSocketAddr =
        SocketAddr::new(IpAddr::V6(sidecar_ip.to_ipv6_mapped()), 1).into();
    let (sidecar, mut sidecar_handle) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .with_connected_addr(ConnectedAddr::new_inbound_direct(sidecar_addr))
        .finish();
    let discovered_peers = stream::iter([Ok::<_, BoxError>(Change::Insert(
        sidecar_addr,
        sidecar.into(),
    ))])
    .chain(stream::pending());
    let (minimum_peer_version, best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Simulate Zebra syncing ahead of its zcashd-compat sidecar, so stall
    // tracking would be active for an ordinary peer.
    best_tip.send_best_tip_height(Some(block::Height(2_490_000)));
    best_tip.send_estimated_distance_to_network_chain_tip(Some(10_000));

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_block_gossip_peer_ips(vec![sidecar_ip.into()])
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .build();

        for _ in 0..FIND_RESPONSE_STALL_THRESHOLD {
            let peer_ready = peer_set.ready().await.expect("peer set is ready");
            let response_fut = peer_ready.call(Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            });
            let client_request = sidecar_handle
                .try_to_receive_outbound_client_request()
                .request()
                .expect("sidecar received the request");
            let _ = client_request.respond(Ok(Response::BlockHashes(vec![])));
            response_fut.await.expect("response received");
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

/// Check that a configured sidecar that is busy with another request when a block
/// is advertised still receives the advert once it becomes ready again.
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

        // Advertise a block while the sidecar is busy: nothing can be sent yet,
        // so the advert must be queued for the sidecar.
        let advert_response = peer_set
            .route_sidecar_broadcast(Request::AdvertiseBlock(block_hash, None))
            .await;
        advert_response.expect("broadcast to zero ready peers succeeds");
        assert!(
            sidecar_handle
                .try_to_receive_outbound_client_request()
                .request()
                .is_none(),
            "busy sidecar must not receive the advert while unready"
        );

        // Complete the in-flight request, making the sidecar ready again.
        let _ = find_blocks_request.respond(Ok(Response::BlockHashes(vec![])));
        find_blocks_fut.await.expect("response received");

        // Polling the peer set delivers the queued advert to the now-ready sidecar.
        let _ = peer_set.ready().await.expect("peer set is ready");
        // Let the detached advert delivery task run.
        tokio::task::yield_now().await;

        let delivered = sidecar_handle
            .try_to_receive_outbound_client_request()
            .request()
            .expect("sidecar received the queued block advert");
        assert_eq!(
            delivered.request,
            Request::AdvertiseBlock(block_hash, None),
            "the queued request must be the block advert"
        );
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
            .with_connected_addr(ConnectedAddr::new_inbound_direct(addr))
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

/// Returns a single block request for a test block hash made from `byte`.
fn block_request(byte: u8) -> Request {
    Request::BlocksByHash(iter::once(block::Hash([byte; 32])).collect())
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

/// Check that block requests that no peer advertised go to a peer that can serve historic blocks,
/// rather than a non-serving peer that is likely to answer `notfound`.
#[test]
fn peer_set_route_block_prefers_serving_peer() {
    peer_set_route_block_prefers_serving_peer_order(true);
    peer_set_route_block_prefers_serving_peer_order(false);
}

fn peer_set_route_block_prefers_serving_peer_order(serving_first: bool) {
    let services = if serving_first {
        [PeerServices::NODE_NETWORK, PeerServices::empty()]
    } else {
        [PeerServices::empty(), PeerServices::NODE_NETWORK]
    };
    let (serving, non_serving) = if serving_first { (0, 1) } else { (1, 0) };

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    let (discovered_peers, _addrs, mut handles) = mock_peers_with_services(&services);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

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

/// A single request survives the old sixteen-second retry budget and reaches a peer ready at
/// eighteen seconds, even after a ready pruned peer rejects the block.
#[test]
fn peer_set_waits_for_busy_block_peer_without_refusal() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    // CORRECTNESS: All peers and deadlines are simulated.
    tokio::time::pause();
    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, mut guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
        let service =
            retry_busy_inventory(tower::buffer::Buffer::new(InventoryService(peer_set), 4));
        let mut response = Box::pin(service.oneshot(block_request(2)));
        assert!(futures::poll!(&mut response).is_pending());
        tokio::task::yield_now().await;

        let request = handles[1]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("ready pruned peer can serve recent blocks");
        let hash = InventoryHash::Block(block::Hash([2; 32]));
        guard
            .inventory_sender()
            .as_mut()
            .unwrap()
            .send(InventoryStatus::new_missing(hash, addrs[1]))
            .unwrap();
        request
            .respond(Err(PeerError::NotFoundResponse(vec![hash]).into()))
            .unwrap();
        assert!(
            timeout(Duration::from_secs(18), &mut response)
                .await
                .is_err(),
            "busy peers must not consume missing-block retries"
        );

        assert_eq!(received_request(&mut handles[0]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(9)));
        // Drive the internal retry; no new request from the caller is needed.
        assert!(timeout(Duration::from_secs(1), &mut response)
            .await
            .is_err());
        let request = handles[0]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("the original request reaches the recovered serving peer");
        assert_eq!(request.request, block_request(2));
        request.respond(Ok(Response::Blocks(vec![]))).unwrap();
        response
            .await
            .expect("recovered peer completes the original request");
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

    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // The busy serving peer advertised the block, so it is a recent block.
        let advertised_inv = InventoryHash::Block(block::Hash([2; 32]));
        peer_set_guard
            .inventory_sender()
            .as_mut()
            .expect("unexpected missing inv sender")
            .send(InventoryStatus::new_available(advertised_inv, addrs[0]))
            .expect("unexpected dropped receiver");

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

    let (discovered_peers, _addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::empty(), PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

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

/// Wait for a busy pruned peer for advertised blocks, but do not wait for unknown historic blocks.
#[test]
fn peer_set_waits_for_advertised_busy_non_serving_peer() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // CORRECTNESS: This test does not depend on external resources that could really timeout.
    tokio::time::pause();

    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (serving_addr, non_serving_addr) = (addrs[0], addrs[1]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // The test inventory channel only holds one change, so poll the peer set to process each
        // change before sending the next one.
        let mut send_inventory_change = |change| {
            peer_set_guard
                .inventory_sender()
                .as_mut()
                .expect("unexpected missing inv sender")
                .send(change)
                .expect("unexpected dropped receiver");
        };

        // Make the non-serving peer busy, by marking the serving peer as missing the requested
        // blocks. The mock peer channel holds 2 requests.
        let mut busy_futs = Vec::new();
        for byte in [1, 9] {
            send_inventory_change(InventoryStatus::new_missing(
                InventoryHash::Block(block::Hash([byte; 32])),
                serving_addr,
            ));
            let peer_ready = peer_set
                .ready()
                .await
                .expect("peer set service is always ready");
            busy_futs.push(peer_ready.call(block_request(byte)));
        }

        // A historic block that the ready serving peer is missing: the busy non-serving peer
        // can't serve it either, so the refusal is instant.
        send_inventory_change(InventoryStatus::new_missing(
            InventoryHash::Block(block::Hash([2; 32])),
            serving_addr,
        ));
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert!(
            peer_ready.cancel_handles.contains_key(&non_serving_addr),
            "non-serving peer should be busy after 2 queued requests",
        );

        let refused_fut = peer_ready.call(block_request(2));
        let response = timeout(Duration::from_millis(100), refused_fut)
            .await
            .expect("a busy non-serving peer should not delay the refusal of a historic block");
        assert_not_found_registry(response);

        // A recent block advertised by the busy pruned peer waits internally, without a refusal.
        let advertised_hash = block::Hash([3; 32]);
        send_inventory_change(InventoryStatus::new_missing(
            InventoryHash::Block(advertised_hash),
            serving_addr,
        ));
        peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        send_inventory_change(InventoryStatus::new_available(
            InventoryHash::Block(advertised_hash),
            non_serving_addr,
        ));
        let service =
            retry_busy_inventory(tower::buffer::Buffer::new(InventoryService(peer_set), 4));
        let mut response = Box::pin(service.oneshot(block_request(3)));
        assert!(timeout(Duration::from_secs(1), &mut response)
            .await
            .is_err());
        assert_eq!(received_request(&mut handles[1]), Some(block_request(1)));
        assert_eq!(received_request(&mut handles[1]), Some(block_request(9)));
        assert!(timeout(Duration::from_secs(1), &mut response)
            .await
            .is_err());
        let request = handles[1]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("the original request reaches the recovered pruned peer");
        assert_eq!(request.request, block_request(3));
        request.respond(Ok(Response::Blocks(vec![]))).unwrap();
        response
            .await
            .expect("pruned peer completes the original request");
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

    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Make the serving peer busy.
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // Mark the requested block as missing on both peers. The test inventory channel only holds
        // one change, so poll the peer set to process each change before sending the next one.
        let missing_inv = InventoryHash::Block(block::Hash([2; 32]));
        for addr in addrs {
            peer_set_guard
                .inventory_sender()
                .as_mut()
                .expect("unexpected missing inv sender")
                .send(InventoryStatus::new_missing(missing_inv, addr))
                .expect("unexpected dropped receiver");
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

        let response = timeout(Duration::from_millis(100), refused_fut)
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

    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Make the serving peer busy.
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;

        // Mark the requested transaction as missing on the ready non-serving peer only.
        let tx_id = UnminedTxId::Legacy(transaction::Hash([3; 32]));
        peer_set_guard
            .inventory_sender()
            .as_mut()
            .expect("unexpected missing inv sender")
            .send(InventoryStatus::new_missing(
                InventoryHash::from(tx_id),
                addrs[1],
            ))
            .expect("unexpected dropped receiver");

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        let refused_fut = peer_ready.call(Request::TransactionsById(iter::once(tx_id).collect()));

        let response = timeout(Duration::from_millis(100), refused_fut)
            .await
            .expect("transaction refusals should not be delayed");
        assert_not_found_registry(response);

        assert_eq!(received_request(&mut handles[1]), None);
    });
}

/// Inventory-channel lag must not prevent a ready gossip source from serving a recent block.
#[test]
fn peer_set_routes_block_after_losing_advertisement() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    let (discovered_peers, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    runtime.block_on(async move {
        let (mut peer_set, mut guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let _busy_futs = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
        let sender = guard.inventory_sender().as_mut().unwrap();
        // Capacity is one: the second announcement overwrites the block we need.
        for byte in [2, 3] {
            sender
                .send(InventoryStatus::new_available(
                    InventoryHash::Block(block::Hash([byte; 32])),
                    addrs[1],
                ))
                .unwrap();
        }
        let _response = peer_set.ready().await.unwrap().call(block_request(2));
        assert_eq!(received_request(&mut handles[1]), Some(block_request(2)));
    });
}

/// A failed or cancelled block request breaks exclusive NODE_NETWORK preference; a later
/// successful block response restores it without poisoning the inventory registry.
#[test]
fn peer_set_block_failure_allows_responsive_fallback() {
    for cancel in [false, true] {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        let (discovered_peers, addrs, mut handles) =
            mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
        let (minimum_peer_version, _best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        runtime.block_on(async move {
            let (mut peer_set, mut guard) = PeerSetBuilder::new()
                .with_discover(discovered_peers)
                .with_minimum_peer_version(minimum_peer_version)
                .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
                .build();
            guard
                .inventory_sender()
                .as_mut()
                .unwrap()
                .send(InventoryStatus::new_available(
                    InventoryHash::Block(block::Hash([2; 32])),
                    addrs[0],
                ))
                .unwrap();
            let response = peer_set.ready().await.unwrap().call(block_request(2));
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            if cancel {
                drop(response);
                request.acknowledge_cancellation();
            } else {
                request
                    .respond(Err(PeerError::ConnectionReceiveTimeout.into()))
                    .unwrap();
                response.await.expect_err("serving peer timed out");
            }
            let response = peer_set.ready().await.unwrap().call(block_request(2));
            let request = handles[1]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("responsive pruned peer must get a chance after a serving-peer failure");
            request.respond(Ok(Response::Blocks(vec![]))).unwrap();
            response.await.unwrap();

            // A failed peer can recover when the other peer is known to lack the next block.
            guard
                .inventory_sender()
                .as_mut()
                .unwrap()
                .send(InventoryStatus::new_missing(
                    InventoryHash::Block(block::Hash([3; 32])),
                    addrs[1],
                ))
                .unwrap();
            let response = peer_set.ready().await.unwrap().call(block_request(3));
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            request.respond(Ok(Response::Blocks(vec![]))).unwrap();
            response.await.unwrap();
            let _response = peer_set.ready().await.unwrap().call(block_request(2));
            assert_eq!(received_request(&mut handles[0]), Some(block_request(2)));
        });
    }
}

/// Bans and protocol upgrades invalidate busy candidates before their queue becomes ready.
#[test]
fn peer_set_does_not_wait_for_ineligible_busy_peers() {
    for ban in [false, true] {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        // CORRECTNESS: All peers and deadlines are simulated.
        tokio::time::pause();
        let addrs: [PeerSocketAddr; 2] = [
            "127.0.0.1:1".parse().unwrap(),
            "127.0.0.2:1".parse().unwrap(),
        ];
        let network = Network::new_regtest(
            zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu6_2: Some(1),
                nu6_3: Some(2),
                ..Default::default()
            }
            .into(),
        );
        let old_version = Version::min_remote_for_height(&network, block::Height(1));
        let (serving, _serving_handle) = ClientTestHarness::build()
            .with_version(old_version)
            .with_services(PeerServices::NODE_NETWORK)
            .finish();
        let (fallback, _fallback_handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_services(PeerServices::empty())
            .finish();
        let discovered = stream::iter([
            Ok::<_, BoxError>(Change::Insert(addrs[0], serving.into())),
            Ok(Change::Insert(addrs[1], fallback.into())),
        ])
        .chain(stream::pending());
        let (minimum_peer_version, best_tip) = MinimumPeerVersion::with_mock_chain_tip(&network);
        best_tip.send_best_tip_height(Some(block::Height(1)));

        runtime.block_on(async move {
            let (mut peer_set, mut guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum_peer_version)
                .build();
            let _busy_futs = timeout(
                Duration::from_secs(1),
                make_serving_peer_busy(&mut peer_set, addrs[0]),
            )
            .await
            .expect("both peers are initially eligible");
            let hash = InventoryHash::Block(block::Hash([2; 32]));
            guard
                .inventory_sender()
                .as_mut()
                .unwrap()
                .send(InventoryStatus::new_missing(hash, addrs[1]))
                .unwrap();
            let mut bans = crate::BanList::default();
            if ban {
                bans.ban(addrs[0].ip());
            } else {
                best_tip.send_best_tip_height(Some(block::Height(2)));
            }
            let (_bans_tx, bans_rx) = watch::channel(bans);
            peer_set.bans_receiver = bans_rx;

            let response = timeout(Duration::from_secs(1), peer_set.ready())
                .await
                .expect("fallback peer remains eligible")
                .unwrap()
                .call(block_request(2));
            assert_not_found_registry(
                timeout(Duration::from_millis(100), response)
                    .await
                    .expect("ineligible busy peers must not delay a refusal"),
            );
        });
    }
}

/// Both advertiser and serving-peer fallback rejections survive a lost registry update.
#[test]
fn peer_set_retries_rejections_after_inventory_loss() {
    for advertised in [false, true] {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();
        tokio::time::pause();
        let (discovered, addrs, mut handles) =
            mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
        let (minimum_peer_version, _best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
        let block: block::Block = zebra_test::vectors::BLOCK_MAINNET_10_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let hash = block.hash();
        let inventory = InventoryHash::Block(hash);
        runtime.block_on(async move {
            let (mut peers, mut guard) = PeerSetBuilder::new()
                .with_discover(discovered)
                .with_minimum_peer_version(minimum_peer_version)
                .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
                .build();
            let sender = guard.inventory_sender().as_mut().unwrap();
            if advertised {
                sender
                    .send(InventoryStatus::new_available(inventory, addrs[0]))
                    .unwrap();
                peers.ready().await.unwrap();
            } else {
                // Lose the real holder's announcement before it reaches the registry.
                sender
                    .send(InventoryStatus::new_available(inventory, addrs[1]))
                    .unwrap();
                sender
                    .send(InventoryStatus::new_available(
                        InventoryHash::Block(block::Hash([3; 32])),
                        addrs[0],
                    ))
                    .unwrap();
            }
            let service =
                retry_busy_inventory(tower::buffer::Buffer::new(InventoryService(peers), 3));
            let mut response = Box::pin(service.oneshot(Request::BlocksByHash([hash].into())));
            assert!(timeout(Duration::from_millis(1), &mut response)
                .await
                .is_err());
            let request = handles[0]
                .try_to_receive_outbound_client_request()
                .request()
                .unwrap();
            request
                .respond(Err(PeerError::NotFoundResponse(vec![inventory]).into()))
                .unwrap();
            // Forward the real response collector update, then overwrite it in the one-slot ring.
            let missing = handles[0].try_to_receive_inventory_change().unwrap();
            assert_eq!(missing, InventoryStatus::new_missing(inventory, addrs[0]));
            sender.send(missing).unwrap();
            sender
                .send(InventoryStatus::new_available(
                    InventoryHash::Block(block::Hash([4; 32])),
                    addrs[0],
                ))
                .unwrap();
            assert!(timeout(Duration::from_millis(1), &mut response)
                .await
                .is_err());
            let request = handles[1]
                .try_to_receive_outbound_client_request()
                .request()
                .expect("the same caller request must reach the ready pruned holder");
            assert_eq!(request.request, Request::BlocksByHash([hash].into()));
            request
                .respond(Ok(Response::Blocks(vec![
                    crate::protocol::internal::InventoryResponse::Available((
                        std::sync::Arc::new(block),
                        Some(addrs[1]),
                    )),
                ])))
                .unwrap();
            let Response::Blocks(blocks) = response.await.unwrap() else {
                panic!("block download returned another response type");
            };
            assert_eq!(blocks[0].available().unwrap().0.hash(), hash);
            assert!(handles[0]
                .try_to_receive_outbound_client_request()
                .is_empty());
        });
    }
}

/// Truthful absence for one hash must not remove archival preference for another hash.
#[test]
fn peer_set_notfound_preserves_unrelated_block_routing() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    let (discovered, _addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    runtime.block_on(async move {
        let (mut peers, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let response = peers.ready().await.unwrap().call(block_request(2));
        handles[0]
            .try_to_receive_outbound_client_request()
            .request()
            .unwrap()
            .respond(Err(PeerError::NotFoundResponse(vec![
                InventoryHash::Block(block::Hash([2; 32])),
            ])
            .into()))
            .unwrap();
        response.await.expect_err("the requested hash is absent");
        let _response = peers.ready().await.unwrap().call(block_request(3));
        assert_eq!(received_request(&mut handles[0]), Some(block_request(3)));
        assert!(handles[1]
            .try_to_receive_outbound_client_request()
            .is_empty());
    });
}

/// A busy alternative does not turn a transport failure into a retry deadline error.
#[test]
fn peer_set_preserves_transport_error_with_busy_alternative() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();
    let (discovered, addrs, mut handles) =
        mock_peers_with_services(&[PeerServices::NODE_NETWORK, PeerServices::empty()]);
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    runtime.block_on(async move {
        let (mut peers, _guard) = PeerSetBuilder::new()
            .with_discover(discovered)
            .with_minimum_peer_version(minimum_peer_version)
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();
        let _busy = make_serving_peer_busy(&mut peers, addrs[0]).await;
        let service = retry_busy_inventory(tower::buffer::Buffer::new(InventoryService(peers), 3));
        let mut response = Box::pin(service.oneshot(block_request(2)));
        assert!(timeout(Duration::from_millis(1), &mut response)
            .await
            .is_err());
        handles[1]
            .try_to_receive_outbound_client_request()
            .request()
            .unwrap()
            .respond(Err(PeerError::ConnectionClosed.into()))
            .unwrap();
        let error = timeout(Duration::from_millis(1), response)
            .await
            .expect("transport errors propagate without waiting for a busy peer")
            .unwrap_err();
        assert!(error
            .downcast_ref::<SharedPeerError>()
            .unwrap()
            .inner_debug()
            .contains("ConnectionClosed"));
    });
}
