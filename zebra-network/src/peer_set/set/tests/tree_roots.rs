//! Tree roots requests only go to peers that serve them.

use super::*;

fn tree_roots_request() -> Request {
    Request::TreeRoots {
        start_height: block::Height(1),
        final_hash: block::Hash([1; 32]),
        count: 1,
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

    // P2C picks randomly, so repeat to catch routing to the other peer.
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
