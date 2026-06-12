//! Fixed test vectors for the peer set.

use std::{
    cmp::max,
    collections::HashSet,
    iter,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures::{future, stream, StreamExt};
use tokio::time::timeout;
use tower::{discover::Change, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserialize,
};

use crate::{
    constants::DEFAULT_MAX_CONNS_PER_IP,
    peer::{ClientRequest, ClientTestHarness, LoadTrackedClient, MinimumPeerVersion},
    peer_set::inventory_registry::InventoryStatus,
    protocol::external::{types::Version, InventoryHash},
    BoxError, InventoryResponse, PeerSocketAddr, Request, Response, SharedPeerError,
};
use indexmap::IndexMap;
use tokio::sync::watch;

use super::{
    super::{track_block_delivery, MorePeers, PeerSetStatus, TIP_STALL_EVICTION_TIMEOUT},
    PeerSetBuilder, PeerVersions,
};

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

/// Check that the peer set evicts a random peer when the best chain tip height
/// has not grown for longer than [`TIP_STALL_EVICTION_TIMEOUT`], and at least
/// one peer reports a height above ours (so there is newer chain we are failing
/// to obtain).
#[test]
fn peer_set_evicts_random_peer_on_stalled_tip() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (minimum_peer_version, best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Set an initial tip height, so the first poll seeds the stall detector.
    best_tip_height.send_best_tip_height(block::Height(100));

    runtime.block_on(async move {
        // Three peers, all reporting a height above our tip: the sync is behind,
        // so a static tip is a genuine stall worth churning a peer over. The
        // harnesses must outlive the peer set — dropping one closes its mock
        // connection and the peer never becomes ready.
        let mut harnesses = Vec::new();
        let discovered_peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> = (1
            ..=3u16)
            .map(|port| {
                let (client, harness) = ClientTestHarness::build()
                    .with_version(peer_version)
                    .with_start_height(block::Height(200))
                    .finish();
                harnesses.push(harness);
                let addr: PeerSocketAddr = SocketAddr::new([127, 0, 0, 1].into(), port).into();
                Ok(Change::Insert(addr, client.into()))
            })
            .collect();
        let discovered_peers = stream::iter(discovered_peers).chain(stream::pending());

        // Build a peerset
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(3, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Drive a first poll to make all peers ready and seed the stall detector
        // with the current tip height.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(peer_ready.ready_services.len(), 3);

        // Pretend the tip last grew more than the eviction timeout ago, while
        // leaving the tip height unchanged so it counts as stalled.
        peer_set.last_tip_growth = Instant::now() - (TIP_STALL_EVICTION_TIMEOUT * 2);

        // Driving another poll should now evict exactly one random peer.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(
            peer_ready.ready_services.len(),
            2,
            "a stalled tip should evict exactly one peer"
        );

        // The eviction should have asked the crawler for a replacement peer.
        assert!(
            matches!(
                peer_set_guard
                    .demand_receiver()
                    .as_mut()
                    .expect("demand receiver is created by the builder")
                    .try_recv(),
                Ok(MorePeers)
            ),
            "evicting a peer should send a MorePeers demand signal"
        );
    });
}

/// Check that the peer set does *not* evict a peer when the tip is static but no
/// peer reports a height above ours — i.e. we are caught up (or on a quiet
/// network), so churning peers can't help. This covers fully-synced nodes
/// during a natural long block gap, and regtest/unmined-testnet nodes whose tip
/// never grows.
#[test]
fn peer_set_keeps_peers_when_caught_up() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (minimum_peer_version, best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    best_tip_height.send_best_tip_height(block::Height(200));

    runtime.block_on(async move {
        // Three peers all at our tip height (none ahead): we are caught up.
        let mut harnesses = Vec::new();
        let discovered_peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> = (1
            ..=3u16)
            .map(|port| {
                let (client, harness) = ClientTestHarness::build()
                    .with_version(peer_version)
                    .with_start_height(block::Height(200))
                    .finish();
                harnesses.push(harness);
                let addr: PeerSocketAddr = SocketAddr::new([127, 0, 0, 1].into(), port).into();
                Ok(Change::Insert(addr, client.into()))
            })
            .collect();
        let discovered_peers = stream::iter(discovered_peers).chain(stream::pending());

        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(3, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(peer_ready.ready_services.len(), 3);

        // Force the stall timer well into the past, with a static tip.
        peer_set.last_tip_growth = Instant::now() - (TIP_STALL_EVICTION_TIMEOUT * 2);

        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(
            peer_ready.ready_services.len(),
            3,
            "a static tip with no peer ahead of us is not a stall worth evicting over"
        );

        assert!(
            peer_set_guard
                .demand_receiver()
                .as_mut()
                .expect("demand receiver is created by the builder")
                .try_recv()
                .is_err(),
            "being caught up should not send a MorePeers demand signal"
        );
    });
}

/// Check that the peer set does *not* evict a peer when the best chain tip height
/// is still growing, even if a long time has passed since the last poll.
#[test]
fn peer_set_keeps_peers_when_tip_grows() {
    // Use three peers with the same version.
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Get peers and client handles of them
    let (discovered_peers, _handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    // Set an initial tip height, so the first poll seeds the stall detector.
    best_tip_height.send_best_tip_height(block::Height(100));

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, mut peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(3, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        // Drive a first poll to make all peers ready and seed the stall detector
        // with the current tip height.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(peer_ready.ready_services.len(), 3);

        // Force the stall timer well into the past, but also grow the tip height.
        peer_set.last_tip_growth = Instant::now() - (TIP_STALL_EVICTION_TIMEOUT * 2);
        best_tip_height.send_best_tip_height(block::Height(101));

        // Driving another poll should keep all the peers: the tip grew, so the
        // stall timer is reset instead of evicting a peer.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(
            peer_ready.ready_services.len(),
            3,
            "a growing tip should not evict any peers"
        );

        // No eviction means no demand for a replacement peer.
        assert!(
            peer_set_guard
                .demand_receiver()
                .as_mut()
                .expect("demand receiver is created by the builder")
                .try_recv()
                .is_err(),
            "a growing tip should not send a MorePeers demand signal"
        );
    });
}

/// Check that the peer set routes multi-block download requests to a ready
/// peer whose chain height is at or above our current chain tip, in preference
/// to ready peers that are behind us.
#[test]
fn peer_set_routes_multi_block_downloads_to_tall_peers() {
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Peer heights are compared against this chain tip height.
    let (minimum_peer_version, best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    best_tip_height.send_best_tip_height(block::Height(100));

    runtime.block_on(async move {
        // A peer that is at or above our chain tip, and a peer that is behind us.
        let (tall_client, mut tall_handle) = ClientTestHarness::build()
            .with_version(peer_version)
            .with_start_height(block::Height(200))
            .finish();
        let (short_client, mut short_handle) = ClientTestHarness::build()
            .with_version(peer_version)
            .with_start_height(block::Height(0))
            .finish();

        let tall_addr: PeerSocketAddr = SocketAddr::new([127, 0, 0, 1].into(), 1).into();
        let short_addr: PeerSocketAddr = SocketAddr::new([127, 0, 0, 1].into(), 2).into();

        let discovered_peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> = vec![
            Ok(Change::Insert(tall_addr, tall_client.into())),
            Ok(Change::Insert(short_addr, short_client.into())),
        ];
        let discovered_peers = stream::iter(discovered_peers).chain(stream::pending());

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

        // A multi-block download request skips inventory routing and uses
        // height-aware routing.
        let sent_request = Request::BlocksByHash(
            [block::Hash([0; 32]), block::Hash([1; 32])]
                .into_iter()
                .collect(),
        );
        let _fut = peer_ready.call(sent_request.clone());

        // Only the peer at or above the chain tip should receive the request.
        if let Some(ClientRequest { request, .. }) = tall_handle
            .try_to_receive_outbound_client_request()
            .request()
        {
            assert_eq!(sent_request, request);
        } else {
            panic!("block download not routed to the peer at or above our chain tip");
        }

        assert!(
            short_handle
                .try_to_receive_outbound_client_request()
                .request()
                .is_none(),
            "block download routed to a peer that is behind our chain tip",
        );
    });
}

/// Check that the peer set raises a peer's height when the peer delivers a
/// block, so height-aware routing uses delivered blocks as well as the
/// handshake height.
#[test]
fn peer_set_tracks_delivered_block_height() {
    // Use one peer, so the request routing is deterministic.
    let peer_versions = PeerVersions {
        peer_versions: vec![Version::min_specified_for_upgrade(
            &Network::Mainnet,
            NetworkUpgrade::Nu6_2,
        )],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Get the peer and its client handle
    let (discovered_peers, mut handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

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
        assert_eq!(peer_ready.ready_services.len(), 1);

        // The mock handshake reports a start height of zero.
        let handshake_height = peer_ready
            .ready_services
            .values()
            .next()
            .expect("just checked there is a ready peer")
            .remote_height();
        assert_eq!(handshake_height, block::Height(0));

        let block = Arc::new(
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .expect("hard-coded test vector deserializes"),
        );
        let block_height = block
            .coinbase_height()
            .expect("hard-coded test vector has a coinbase height");

        // Request the block, and deliver it through the mocked connection.
        let response_fut =
            peer_ready.call(Request::BlocksByHash(iter::once(block.hash()).collect()));

        let client_request = handles[0]
            .try_to_receive_outbound_client_request()
            .request()
            .expect("peer set routes the request to the only ready peer");

        client_request
            .tx
            .send(Ok(Response::Blocks(vec![InventoryResponse::Available((
                block, None,
            ))])))
            .expect("response receiver is held by the routed response future");

        response_fut
            .await
            .expect("delivered block response succeeds");

        // Once the peer becomes ready again, the delivered block has raised
        // its height above the stale handshake height, and is counted in the
        // peer's block-download diagnostics.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");

        let peer = peer_ready
            .ready_services
            .values()
            .next()
            .expect("the peer becomes ready again after responding");
        assert_eq!(peer.remote_height(), block_height);
        assert_eq!(peer.blocks_received(), 1);
    });
}

/// Check that the block-delivery response wrapper counts delivered blocks and
/// raises the live height, ignoring missing inventory entries.
#[test]
fn track_block_delivery_counts_blocks_and_raises_height() {
    let (runtime, _init_guard) = zebra_test::init_async();

    runtime.block_on(async move {
        let live_height = Arc::new(AtomicU32::new(0));
        let blocks_received = Arc::new(AtomicU64::new(0));

        let block = Arc::new(
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .expect("hard-coded test vector deserializes"),
        );
        let block_height = block
            .coinbase_height()
            .expect("hard-coded test vector has a coinbase height");

        // A partial response: one delivered block and one missing entry.
        let response = Response::Blocks(vec![
            InventoryResponse::Available((block, None)),
            InventoryResponse::Missing(block::Hash([0; 32])),
        ]);

        track_block_delivery(
            live_height.clone(),
            blocks_received.clone(),
            future::ready(Ok::<_, SharedPeerError>(response)),
        )
        .await
        .expect("wrapped response future returns the response");

        // Only the available block counts; the missing entry is ignored.
        assert_eq!(blocks_received.load(Ordering::Relaxed), 1);
        assert_eq!(live_height.load(Ordering::Relaxed), block_height.0);
    });
}

/// Check that the peer set publishes [`PeerSetStatus`] snapshots once peers
/// become ready.
#[test]
fn peer_set_publishes_status_watch() {
    // Use two peers with the same version.
    let peer_version = Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
    let peer_versions = PeerVersions {
        peer_versions: vec![peer_version, peer_version],
    };

    // Start the runtime
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // Get peers and client handles of them
    let (discovered_peers, _handles) = peer_versions.mock_peer_discovery();
    let (minimum_peer_version, _best_tip_height) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);

    runtime.block_on(async move {
        // Build a peerset
        let (mut peer_set, _peer_set_guard) = PeerSetBuilder::new()
            .with_discover(discovered_peers)
            .with_minimum_peer_version(minimum_peer_version.clone())
            .max_conns_per_ip(max(2, DEFAULT_MAX_CONNS_PER_IP))
            .build();

        let status_receiver = peer_set.status_receiver();

        // Before the first poll, the status is empty.
        assert_eq!(*status_receiver.borrow(), PeerSetStatus::default());

        // After a poll, the status reflects the ready peers.
        let peer_ready = peer_set
            .ready()
            .await
            .expect("peer set service is always ready");
        assert_eq!(peer_ready.ready_services.len(), 2);

        let status = *status_receiver.borrow();
        assert_eq!(status.ready_peers, 2);
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
