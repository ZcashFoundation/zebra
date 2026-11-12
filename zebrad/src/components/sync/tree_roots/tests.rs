//! Tests for fetching tree roots from peers.

use zebra_chain::{block, parallel::tree::TreeRoots};
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
            }),
            "wanted: {wanted:?}",
        );
    }
}

/// The fetcher retries refused requests, and supplies the roots it gets to the state.
#[tokio::test(start_paused = true)]
async fn fetch_tree_roots_supplies_peer_roots_to_state() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let wanted = Height(1)..=Height(20_000);
    let request = tree_roots_request(&network.checkpoint_list(), wanted.clone())
        .expect("Mainnet has checkpoints after height 1");
    let roots = vec![TreeRoots::empty(); 2];

    let mut peers = MockService::build().for_unit_tests();
    let mut state = MockService::build().for_unit_tests();
    let task = tokio::spawn(fetch_tree_roots(network, peers.clone(), state.clone()));

    for response in [
        zn::Response::TreeRoots(None),
        zn::Response::TreeRoots(Some(roots.clone())),
    ] {
        state
            .expect_request(zs::Request::WantedTreeRoots)
            .await
            .respond(zs::Response::WantedTreeRoots(Some(wanted.clone())));
        peers
            .expect_request(request.clone())
            .await
            .respond(response);
    }
    state
        .expect_request(zs::Request::SupplyTreeRoots {
            start_height: Height(1),
            roots,
        })
        .await
        .respond(zs::Response::TreeRootsSupplied);

    task.abort();
}
