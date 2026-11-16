//! Retry-dispatch liveness through the complete sync consumer.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
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
    components::sync::{
        downloads::BlockDownloadVerifyError, ChainSync, BLOCK_DOWNLOAD_TIMEOUT,
        BLOCK_VERIFY_TIMEOUT, FANOUT,
    },
    config::ZebradConfig,
};

/// Slow retained retries with empty fresh tips share the round deadline and retain unsent hashes.
#[tokio::test(start_paused = true)]
async fn retained_retries_share_round_progress_deadline() {
    let _init_guard = zebra_test::init();
    let hashes: Vec<_> = (1..=32).map(|byte| block::Hash([byte; 32])).collect();
    let requested = Arc::new(Mutex::new(HashSet::new()));
    let block_requests = requested.clone();
    let tips = Arc::new(AtomicUsize::new(0));
    let tip_requests = tips.clone();
    let network = tower::service_fn(move |request| {
        let block_requests = block_requests.clone();
        let tip_requests = tip_requests.clone();
        async move {
            match request {
                zn::Request::FindBlocks { .. } => {
                    tip_requests.fetch_add(1, Ordering::SeqCst);
                    Ok(zn::Response::BlockHashes(Vec::new()))
                }
                zn::Request::BlockDownload { hash, .. } => {
                    block_requests.lock().unwrap().insert(hash);
                    tokio::time::sleep(BLOCK_DOWNLOAD_TIMEOUT / 2).await;
                    Err(crate::BoxError::from("connection reset"))
                }
                request => panic!("unexpected network request: {request:?}"),
            }
        }
    });
    let state = tower::service_fn(|request| {
        assert!(matches!(request, zs::Request::BlockLocator));
        std::future::ready(Ok::<_, crate::BoxError>(zs::Response::BlockLocator(vec![
            block::Hash([0; 32]),
        ])))
    });
    let mut config = ZebradConfig::default();
    config.sync.download_concurrency_limit = 1;
    let (tip, _tip_sender) = MockChainTip::new();
    let (misbehavior, _reports) = tokio::sync::mpsc::channel(1);
    let (mut sync, _status) = ChainSync::new(
        &config,
        Height(1_000_000),
        network,
        MockService::build().for_unit_tests(),
        state,
        MockService::build().for_unit_tests(),
        tip,
        misbehavior,
    );
    for hash in &hashes {
        sync.handle_block_response(Err(BlockDownloadVerifyError::DownloadTimedOut {
            hash: *hash,
        }))
        .unwrap();
    }

    let started = tokio::time::Instant::now();
    tokio::time::timeout(
        BLOCK_VERIFY_TIMEOUT + Duration::from_secs(1),
        sync.try_to_sync(),
    )
    .await
    .expect("retained retry dispatch must obey the round progress deadline")
    .expect("a no-progress round must back off without resetting retained work");
    assert_eq!(started.elapsed(), BLOCK_VERIFY_TIMEOUT);
    assert_eq!(tips.load(Ordering::SeqCst), FANOUT);

    let requested = requested.lock().unwrap();
    assert!(!requested.is_empty(), "retained retries must be dispatched");
    let unsent: Vec<_> = hashes
        .iter()
        .filter(|hash| !requested.contains(*hash))
        .copied()
        .collect();
    assert!(
        !unsent.is_empty(),
        "the slow backlog must outlast one round"
    );
    assert!(
        unsent
            .iter()
            .all(|hash| sync.reobtain_hashes.contains(hash)),
        "cancelling dispatch must preserve every unsent hash"
    );
    assert!(
        hashes
            .iter()
            .all(|hash| sync.block_reobtain_retries.get(hash) == Some(&1)),
        "backing off must preserve retry budgets for queued and unsent hashes"
    );
    drop(requested);
    sync.downloads.cancel_all();
}
