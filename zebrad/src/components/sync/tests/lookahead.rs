//! Regression tests for lookahead admission during concurrent chain progress.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use zebra_chain::{
    block::{self, Height},
    chain_tip::mock::MockChainTip,
};
use zebra_network as zn;
use zebra_state as zs;
use zebra_test::mock_service::MockService;

use crate::{components::sync::ChainSync, config::ZebradConfig};

/// Concurrent commits remove only completed downloads, preserving unknown-parent work and order.
#[tokio::test]
async fn obtain_tips_tolerates_commits_during_fanout() {
    let _init_guard = zebra_test::init();
    let parent = block::Hash([0; 32]);
    let hashes = [
        block::Hash([1; 32]),
        block::Hash([2; 32]),
        block::Hash([3; 32]),
        block::Hash([4; 32]),
    ];
    let advanced = Arc::new(AtomicBool::new(false));
    let state = tower::service_fn(move |request| {
        let response = match request {
            zs::Request::BlockLocator => zs::Response::BlockLocator(vec![parent]),
            zs::Request::KnownBlock(hash) => {
                let before_commit = hash == hashes[0] && !advanced.swap(true, Ordering::SeqCst);
                let known = if before_commit {
                    None
                } else if hashes[..2].contains(&hash) {
                    Some(zs::KnownBlock::BestChain)
                } else if hash == hashes[3] {
                    Some(zs::KnownBlock::Queue)
                } else {
                    None
                };
                zs::Response::KnownBlock(known)
            }
            request => panic!("unexpected state request: {request:?}"),
        };
        std::future::ready(Ok::<_, zs::BoxError>(response))
    });
    let (downloads, mut requested) = tokio::sync::mpsc::channel(4);
    let network = tower::service_fn(move |request| {
        let downloads = downloads.clone();
        async move {
            match request {
                zn::Request::FindBlocks { .. } => {
                    Ok::<_, zn::BoxError>(zn::Response::BlockHashes(hashes.to_vec()))
                }
                zn::Request::BlockDownload {
                    hash,
                    advertiser: None,
                } => {
                    downloads.send(hash).await.unwrap();
                    std::future::pending().await
                }
                request => panic!("unexpected network request: {request:?}"),
            }
        }
    });
    let (tip, _tip_sender) = MockChainTip::new();
    let (misbehavior, _reports) = tokio::sync::mpsc::channel(1);
    let (mut sync, _status) = ChainSync::new(
        &ZebradConfig::default(),
        Height(0),
        network,
        MockService::build().for_unit_tests(),
        state,
        MockService::build().for_unit_tests(),
        tip,
        misbehavior,
    );
    tokio::time::timeout(Duration::from_secs(5), sync.obtain_tips())
        .await
        .expect("tip discovery is bounded")
        .expect("commits during fanout are progress, not a sync failure");
    let requests = tokio::time::timeout(Duration::from_secs(5), async {
        [
            requested.recv().await.unwrap(),
            requested.recv().await.unwrap(),
        ]
    })
    .await
    .expect("both remaining downloads must be dispatched");
    assert_eq!(requests, [hashes[2], hashes[3]]);
    assert!(
        requested.try_recv().is_err(),
        "committed hashes must not consume download capacity"
    );
    sync.downloads.cancel_all();
}
