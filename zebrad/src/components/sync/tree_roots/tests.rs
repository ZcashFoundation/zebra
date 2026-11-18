//! Tests for fetching tree roots from peers.

use zebra_chain::{
    block,
    parallel::tree::{TreeRoots, TreeRootsProof},
};
use zebra_test::mock_service::MockService;

use super::*;

/// Returns the test checkpoint hash at `height`.
fn hash(height: u32) -> block::Hash {
    let mut hash = [1; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    block::Hash(hash)
}

/// Requests end at the highest checkpoint within the wanted range and the request limit, or at the
/// lowest checkpoint within the limit if there is none.
#[test]
fn tree_roots_request_ends_at_a_checkpoint() {
    let checkpoints = CheckpointList::from_list(
        [0, 100, 3_000, 4_100, 4_500].map(|height| (Height(height), hash(height))),
    )
    .expect("the test checkpoints are valid");

    for (wanted, expected) in [
        // The highest checkpoint within the wanted range and the limit.
        (1..=20_000, Some((3_000, 3_000))),
        (100..=20_000, Some((3_000, 2_901))),
        // The limit is inclusive.
        (101..=20_000, Some((4_100, 4_000))),
        // No checkpoint in the wanted range, so the next one.
        (1..=50, Some((100, 100))),
        (3_001..=3_005, Some((4_100, 1_100))),
        // No checkpoint within the limit.
        (4_501..=5_000, None),
    ] {
        let start_height = Height(*wanted.start());
        assert_eq!(
            tree_roots_request(&checkpoints, start_height..=Height(*wanted.end()),),
            expected.map(|(final_height, count)| zn::Request::TreeRoots {
                start_height,
                final_hash: hash(final_height),
                count,
                excluded_peer: None,
            }),
            "wanted: {wanted:?}",
        );
    }
}

/// The fetcher waits for queued roots to reach the writer, resumes at its next wanted height, and
/// retries refusals and rejected suppliers.
#[tokio::test(start_paused = true)]
async fn fetch_tree_roots_supplies_peer_roots_to_state() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let wanted = Height(1)..=Height(20_000);
    let checkpoints = network.checkpoint_list();
    let request = tree_roots_request(&checkpoints, wanted.clone())
        .expect("Mainnet has checkpoints after height 1");
    let roots = vec![
        TreeRootsProof {
            roots: TreeRoots::empty(),
            successor_auth_data_root: None
        };
        2
    ];

    let mut peers = MockService::build().for_unit_tests();
    let mut state = MockService::build().for_unit_tests();
    let (misbehavior, mut reports) = tokio::sync::mpsc::channel(1);
    let source: zn::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let task = tokio::spawn(fetch_tree_roots(
        network,
        peers.clone(),
        state.clone(),
        misbehavior,
    ));

    for response in [
        zn::Response::TreeRoots(None),
        zn::Response::TreeRootsFromPeer(roots.clone(), source),
    ] {
        state
            .expect_request(zs::Request::WantedTreeRoots)
            .await
            .respond(zs::Response::WantedTreeRoots(Some(wanted.clone()), None));
        peers
            .expect_request(request.clone())
            .await
            .respond(response);
    }
    state
        .expect_request(zs::Request::SupplyTreeRoots {
            start_height: Height(1),
            roots: roots.clone(),
            source: *source,
        })
        .await
        .respond(zs::Response::TreeRootsSupplied);

    for _ in 0..2 {
        state
            .expect_request(zs::Request::WantedTreeRoots)
            .await
            .respond(zs::Response::WantedTreeRoots(Some(wanted.clone()), None));
    }
    let wanted = Height(3)..=Height(20_000);
    let request = tree_roots_request(&checkpoints, wanted.clone())
        .expect("Mainnet has checkpoints after height 3");
    state
        .expect_request(zs::Request::WantedTreeRoots)
        .await
        .respond(zs::Response::WantedTreeRoots(Some(wanted.clone()), None));
    peers
        .expect_request(request.clone())
        .await
        .respond(zn::Response::TreeRootsFromPeer(roots.clone(), source));
    state
        .expect_request(zs::Request::SupplyTreeRoots {
            start_height: Height(3),
            roots: roots.clone(),
            source: *source,
        })
        .await
        .respond(zs::Response::TreeRootsSupplied);

    state
        .expect_request(zs::Request::WantedTreeRoots)
        .await
        .respond(zs::Response::WantedTreeRoots(
            Some(wanted.clone()),
            Some(*source),
        ));
    let report = timeout(Duration::from_secs(5), reports.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.addr, source);
    assert_eq!(report.score, 100);
    let mut retry = request.clone();
    if let zn::Request::TreeRoots { excluded_peer, .. } = &mut retry {
        *excluded_peer = Some(source);
    }
    let alternate: zn::PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();
    let retry_roots = roots.clone();
    peers
        .expect_request(retry)
        .await
        .respond(zn::Response::TreeRootsFromPeer(
            retry_roots.clone(),
            alternate,
        ));
    state
        .expect_request(zs::Request::SupplyTreeRoots {
            start_height: Height(3),
            roots: retry_roots,
            source: *alternate,
        })
        .await
        .respond(zs::Response::TreeRootsSupplied);

    task.abort();
}

/// Short replies keep the only serving peer eligible and never overwrite a rejected source.
#[tokio::test(start_paused = true)]
async fn fetch_tree_roots_keeps_short_reply_peers_and_rejected_source_exclusion() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let source: zn::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let rejected_source: zn::PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();
    let roots = vec![TreeRootsProof {
        roots: TreeRoots::empty(),
        successor_auth_data_root: None,
    }];
    for rejected in [None, Some(rejected_source)] {
        let mut peers = MockService::build().for_unit_tests();
        let mut state = MockService::build().for_unit_tests();
        let (misbehavior, mut reports) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(fetch_tree_roots(
            network.clone(),
            peers.clone(),
            state.clone(),
            misbehavior,
        ));
        for height in 1..=3 {
            let wanted = Height(height)..=Height(20_000);
            let mut request =
                tree_roots_request(&network.checkpoint_list(), wanted.clone()).unwrap();
            if let zn::Request::TreeRoots { excluded_peer, .. } = &mut request {
                *excluded_peer = rejected;
            }
            state
                .expect_request(zs::Request::WantedTreeRoots)
                .await
                .respond(zs::Response::WantedTreeRoots(
                    Some(wanted),
                    rejected.filter(|_| height == 1).map(|source| *source),
                ));
            peers
                .expect_request(request)
                .await
                .respond(zn::Response::TreeRootsFromPeer(roots.clone(), source));
            state
                .expect_request(zs::Request::SupplyTreeRoots {
                    start_height: Height(height),
                    roots: roots.clone(),
                    source: *source,
                })
                .await
                .respond(zs::Response::TreeRootsSupplied);
        }
        if let Some(rejected) = rejected {
            let report = timeout(Duration::from_secs(5), reports.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(report.addr, rejected);
            assert_eq!(report.score, 100);
        }
        assert!(
            reports.try_recv().is_err(),
            "valid prefixes are not misbehavior"
        );
        task.abort();
    }
}
