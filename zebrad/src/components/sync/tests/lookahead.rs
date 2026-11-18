//! Regression tests for lookahead admission during concurrent chain progress.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
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

use crate::{
    components::sync::{ChainSync, FANOUT},
    config::ZebradConfig,
};

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
        block::Hash([5; 32]),
        block::Hash([6; 32]),
    ];
    let batches = Arc::new(AtomicUsize::new(0));
    let state_batches = batches.clone();
    let state = tower::service_fn(move |request| {
        let response = match request {
            zs::Request::BlockLocator => zs::Response::BlockLocator(vec![parent]),
            zs::Request::KnownBlocks(requested) => {
                let batch = state_batches.fetch_add(1, Ordering::SeqCst);
                let known = if batch < FANOUT {
                    assert_eq!(requested, hashes);
                    vec![
                        Some(zs::KnownBlock::WriteChannel),
                        Some(zs::KnownBlock::Queue),
                        Some(zs::KnownBlock::BestChain),
                        None,
                        Some(zs::KnownBlock::Queue),
                        None,
                    ]
                } else {
                    assert_eq!(batch, FANOUT, "only one recheck batch is needed");
                    assert_eq!(requested, hashes[1..]);
                    vec![
                        Some(zs::KnownBlock::BestChain),
                        Some(zs::KnownBlock::WriteChannel),
                        None,
                        Some(zs::KnownBlock::Queue),
                        None,
                    ]
                };
                zs::Response::KnownBlocks(known)
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
    assert_eq!(batches.load(Ordering::SeqCst), FANOUT + 1);
    let requests = tokio::time::timeout(Duration::from_secs(5), async {
        [
            requested.recv().await.unwrap(),
            requested.recv().await.unwrap(),
            requested.recv().await.unwrap(),
        ]
    })
    .await
    .expect("all remaining downloads must be dispatched");
    assert_eq!(requests, [hashes[3], hashes[4], hashes[5]]);
    assert!(
        requested.try_recv().is_err(),
        "committed hashes must not consume download capacity"
    );
    sync.downloads.cancel_all();
}
