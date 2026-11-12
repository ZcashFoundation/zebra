//! Tree roots requests only go to peers that serve them.

use super::*;
use zebra_chain::parallel::tree::{TreeRoots, TreeRootsProof};

fn root_proof() -> TreeRootsProof {
    TreeRootsProof {
        roots: TreeRoots::empty(),
        successor_auth_data_root: None,
    }
}

fn tree_roots_request() -> Request {
    Request::TreeRoots {
        start_height: block::Height(1),
        final_hash: block::Hash([1; 32]),
        count: 1,
        excluded_peer: None,
    }
}

/// Tree roots requests go to a ready `NODE_TREE_ROOTS` peer, and fail at once without one.
#[tokio::test(start_paused = true)]
async fn peer_set_routes_tree_roots_only_to_serving_peers() {
    let _init_guard = zebra_test::init();
    let (mut peer_set, _guard, _addrs, mut handles, _best_tip) = peer_set_with_services(&[
        PeerServices::NODE_NETWORK,
        PeerServices::NODE_NETWORK | PeerServices::NODE_TREE_ROOTS,
    ]);

    // Completed refusals must leave the eligible connection available.
    for _ in 0..8 {
        let response = Response::TreeRoots(None);
        assert_eq!(
            respond_to_request(
                &mut peer_set,
                &mut handles[1],
                tree_roots_request(),
                Ok(response.clone()),
            )
            .await
            .expect("the serving peer answers"),
            response,
        );
        assert_eq!(received_request(&mut handles[0]), None);
    }

    let (mut peer_set, _guard, _addrs, mut handles, _best_tip) =
        peer_set_with_services(&[PeerServices::NODE_NETWORK]);
    let error = timeout(Duration::from_secs(1), async {
        peer_set
            .ready()
            .await
            .expect("the peer set is ready")
            .call(tree_roots_request())
            .await
    })
    .await
    .expect("tree roots requests must not queue")
    .expect_err("no ready peer serves tree roots");
    let error = error
        .downcast_ref::<SharedPeerError>()
        .expect("peer set returns a boxed SharedPeerError");
    assert!(error.inner_debug().contains("NoReadyPeers"), "{error:?}");
    assert_eq!(received_request(&mut handles[0]), None);
}

#[tokio::test(start_paused = true)]
async fn rejected_root_source_is_excluded_and_alternate_response_is_attributed() {
    let _init_guard = zebra_test::init();
    let services = PeerServices::NODE_NETWORK | PeerServices::NODE_TREE_ROOTS;
    let (mut peer_set, _guard, addrs, mut handles, _best_tip) =
        peer_set_with_services(&[services, services]);
    let mut request = tree_roots_request();
    if let Request::TreeRoots { excluded_peer, .. } = &mut request {
        *excluded_peer = Some(addrs[0]);
    }
    let roots = vec![root_proof()];
    let response = respond_to_request(
        &mut peer_set,
        &mut handles[1],
        request,
        Ok(Response::TreeRoots(Some(roots.clone()))),
    )
    .await
    .unwrap();
    assert_eq!(response, Response::TreeRootsFromPeer(roots, addrs[1]));
    assert_eq!(received_request(&mut handles[0]), None);

    let (mut peer_set, _guard, addrs, mut handles, _best_tip) = peer_set_with_services(&[services]);
    let mut request = tree_roots_request();
    if let Request::TreeRoots { excluded_peer, .. } = &mut request {
        *excluded_peer = Some(addrs[0]);
    }
    let result = timeout(Duration::from_secs(1), async {
        peer_set.ready().await.unwrap().call(request).await
    })
    .await
    .expect("an excluded-only peer set must refuse without waiting");
    assert!(result.is_err());
    assert_eq!(received_request(&mut handles[0]), None);
}

/// Refusals and empty prefixes cannot starve a slower peer, even across unrelated discovery,
/// request completion, source exclusion, and replacement at a reused address.
#[tokio::test(start_paused = true)]
async fn tree_roots_rotate_past_fast_refusers_and_reconnections() {
    let _init_guard = zebra_test::init();
    let services = PeerServices::NODE_NETWORK | PeerServices::NODE_TREE_ROOTS;
    let mut clients = Vec::<LoadTrackedClient>::new();
    let mut handles = Vec::new();
    for _ in 0..3 {
        let (client, handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_services(services)
            .finish();
        clients.push(client.into());
        handles.push(handle);
    }
    let addrs: Vec<_> = (1..=3)
        .map(|port| PeerSocketAddr::from(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)))
        .collect();
    let (mut discovery_tx, discovered) = mpsc::channel(4);
    for (addr, client) in addrs.iter().copied().zip(clients) {
        discovery_tx
            .try_send(Ok::<_, BoxError>(Change::Insert(addr, client)))
            .unwrap();
    }
    let (minimum_peer_version, _best_tip) =
        MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (mut peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(discovered)
        .with_minimum_peer_version(minimum_peer_version)
        .max_conns_per_ip(3)
        .build();

    peer_set.ready().await.unwrap();
    let warmup = peer_set.call_ready_peer(addrs[2], Request::Peers);
    let warmup_request = handles[2]
        .try_to_receive_outbound_client_request()
        .request()
        .unwrap();
    tokio::time::advance(Duration::from_secs(60)).await;
    warmup_request.tx.send(Ok(Response::Peers(vec![]))).unwrap();
    warmup.await.unwrap();
    peer_set.ready().await.unwrap();
    for addr in &addrs[..2] {
        assert!(
            peer_set.query_load(addr).unwrap() < peer_set.query_load(&addrs[2]).unwrap(),
            "P2C would always prefer a fast refuser over the capable peer"
        );
    }

    let roots = vec![root_proof()];
    for turn in 0..9 {
        let index = turn % 3;
        let reply = match index {
            0 => None,
            1 => Some(vec![]),
            _ => Some(roots.clone()),
        };
        let expected = match &reply {
            Some(roots) => Response::TreeRootsFromPeer(roots.clone(), addrs[index]),
            None => Response::TreeRoots(None),
        };
        assert_eq!(
            respond_to_request(
                &mut peer_set,
                &mut handles[index],
                tree_roots_request(),
                Ok(Response::TreeRoots(reply)),
            )
            .await
            .unwrap(),
            expected,
        );
        // Find traffic has its own cursor; sharing that cursor would skip a root source.
        respond_to_request(
            &mut peer_set,
            &mut handles[index],
            Request::FindBlocks {
                known_blocks: vec![],
                stop: None,
            },
            Ok(Response::BlockHashes(vec![])),
        )
        .await
        .unwrap();
    }

    let mut excluded = tree_roots_request();
    if let Request::TreeRoots { excluded_peer, .. } = &mut excluded {
        *excluded_peer = Some(addrs[0]);
    }
    for index in [1, 2, 1, 2] {
        respond_to_request(
            &mut peer_set,
            &mut handles[index],
            excluded.clone(),
            Ok(Response::TreeRoots(None)),
        )
        .await
        .unwrap();
        assert!(handles[index].wants_connection_heartbeats());
    }

    // The next source was address 0, but its replacement must join behind surviving peers.
    for replacement_services in [services, PeerServices::NODE_NETWORK] {
        let (replacement, replacement_handle) = ClientTestHarness::build()
            .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
            .with_services(replacement_services)
            .finish();
        discovery_tx.try_send(Ok(Change::Remove(addrs[0]))).unwrap();
        discovery_tx
            .try_send(Ok(Change::Insert(addrs[0], replacement.into())))
            .unwrap();
        let mut old_handle = std::mem::replace(&mut handles[0], replacement_handle);
        let order = if replacement_services.contains(PeerServices::NODE_TREE_ROOTS) {
            vec![1, 2, 0]
        } else {
            vec![1, 2, 1, 2]
        };
        for index in order {
            respond_to_request(
                &mut peer_set,
                &mut handles[index],
                tree_roots_request(),
                Ok(Response::TreeRoots(None)),
            )
            .await
            .unwrap();
        }
        assert!(!old_handle.wants_connection_heartbeats());
        assert_eq!(
            peer_set.tree_root_peer_queue.len(),
            if replacement_services.contains(PeerServices::NODE_TREE_ROOTS) {
                3
            } else {
                2
            },
            "rotation contains exactly the live root-serving connections",
        );
    }
}
