//! Tests for the sync pipeline's byte bound (design §4.2, §6.1 "Pipeline (B)").

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use chrono::DateTime;
use futures::stream::StreamExt;
use indexmap::IndexSet;
use tokio::time::Instant;

use zebra_chain::{
    block::{self, Block, Height, MAX_BLOCK_BYTES},
    chain_tip::mock::MockChainTip,
    parameters::checkpoint::list::CheckpointList,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::{LockTime, Transaction},
    transparent,
};
use zebra_network::{self as zn, InventoryResponse::Available};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion, ResponseSender};

use crate::{
    components::sync::{
        downloads::{BlockDownloadVerifyError, Downloads},
        ChainSync, BLOCK_DOWNLOAD_ROUND_TIMEOUT, MIN_CHECKPOINT_CONCURRENCY_LIMIT,
    },
    config::ZebradConfig,
};

use super::*;

/// Maximum time to wait for a request to any test service.
///
/// The tests pause Tokio time, so this window costs no idle time.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

type MockNetwork = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type MockReadState = MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>;
type TestDownloads = Downloads<MockNetwork, MockVerifier, MockReadState, MockChainTip>;

/// Returns the Mainnet block at `height` from the test vectors.
fn mainnet_block(height: u32) -> Arc<Block> {
    zebra_test::vectors::MAINNET_BLOCKS
        .get(&height)
        .expect("the test vectors contain the first ten Mainnet blocks")
        .zcash_deserialize_into()
        .expect("hard-coded block vectors deserialize")
}

/// Returns a checkpoint list with checkpoints at the Mainnet blocks at heights 0, 5 and 10.
fn checkpoints() -> Arc<CheckpointList> {
    Arc::new(
        CheckpointList::from_list(
            [0, 5, 10].map(|height| (Height(height), mainnet_block(height).hash())),
        )
        .expect("the checkpoint list starts at genesis and has unique entries"),
    )
}

/// Returns a byte bound of `pipeline_bytes`.
fn bounded(pipeline_bytes: u64) -> PipelineBudget {
    PipelineBudget::new(SyncMemoryLimits {
        pipeline_bytes: Some(pipeline_bytes),
    })
}

/// Builds a sync downloader with a state tip at genesis, the checkpoints of [`checkpoints`], a
/// last checkpoint at `max_checkpoint_height`, and a byte bound of `pipeline_bytes`.
fn bounded_downloads(
    max_checkpoint_height: Height,
    pipeline_bytes: u64,
) -> (TestDownloads, MockNetwork, MockVerifier) {
    let network: MockNetwork = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let verifier: MockVerifier = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let read_state: MockReadState = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let (latest_chain_tip, chain_tip_sender) = MockChainTip::new();
    chain_tip_sender.send_best_tip_height(Height(0));

    let (past_lookahead_limit_sender, _past_lookahead_limit_receiver) =
        tokio::sync::watch::channel(false);
    let admission = PipelineAdmission::new(
        bounded(pipeline_bytes),
        checkpoints(),
        max_checkpoint_height,
    )
    .expect("a bounded budget has an admission state");

    let downloads = Downloads::new(
        network.clone(),
        verifier.clone(),
        read_state,
        latest_chain_tip,
        past_lookahead_limit_sender,
        MIN_CHECKPOINT_CONCURRENCY_LIMIT,
        max_checkpoint_height,
    )
    .with_pipeline_admission(admission);

    (downloads, network, verifier)
}

/// Queues `block`, serves its download, and returns its pending verification.
///
/// The verifier receives the block after its charge is added, so the charge includes it.
async fn deliver(
    downloads: &mut TestDownloads,
    network: &mut MockNetwork,
    verifier: &mut MockVerifier,
    block: &Arc<Block>,
) -> ResponseSender<zebra_consensus::Request, block::Hash, zn::BoxError> {
    let hash = block.hash();
    downloads
        .download_and_verify(hash)
        .await
        .expect("the download is admitted");
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((block.clone(), None))]));

    verifier
        .expect_request(zebra_consensus::Request::Commit(block.clone()))
        .await
}

/// Returns `true` if `error` is a deferred download of `expected`.
fn is_deferred(error: &BlockDownloadVerifyError, expected: block::Hash) -> bool {
    matches!(error, BlockDownloadVerifyError::PipelineFull { hash } if *hash == expected)
}

/// Without a memory budget the pipeline has no byte bound, so the syncer never charges, waits or
/// defers, and the inbound downloader never drops a block (design §2.1, D1).
#[tokio::test]
async fn default_limits_leave_the_pipeline_unbounded() {
    let _init_guard = zebra_test::init();

    let budget = PipelineBudget::new(SyncMemoryLimits::default());
    assert!(!budget.is_bounded());
    assert_eq!(budget.limit(), None);
    assert!(budget.charge(u64::MAX).is_none());
    assert_eq!(budget.charged_bytes(), 0);
    assert!(!budget.is_over_limit());
    assert!(!PipelineBudget::default().is_bounded());
    budget.update_limit(1);
    assert_eq!(budget.limit(), None);
    assert!(PipelineAdmission::new(budget, checkpoints(), Height(10)).is_none());

    let config = ZebradConfig::default();
    for with_limits in [false, true] {
        let (tip, _tip_sender) = MockChainTip::new();
        let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
        let network: MockNetwork = MockService::build().for_unit_tests();
        let verifier: MockVerifier = MockService::build().for_unit_tests();
        let state: MockService<zs::Request, zs::Response, PanicAssertion> =
            MockService::build().for_unit_tests();
        let read_state: MockReadState = MockService::build().for_unit_tests();

        let (chain_sync, _status) = if with_limits {
            ChainSync::new_with_limits(
                &config,
                Height(10),
                network,
                verifier,
                state,
                read_state,
                tip,
                misbehavior_tx,
                checkpoints(),
                SyncMemoryLimits::default(),
            )
        } else {
            ChainSync::new(
                &config,
                Height(10),
                network,
                verifier,
                state,
                read_state,
                tip,
                misbehavior_tx,
            )
        };

        assert!(!chain_sync.pipeline_budget().is_bounded());
        assert!(chain_sync.downloads.admission().is_none());
    }
}

/// Charges add up, and each guard releases exactly its own bytes and wakes waiting downloads.
#[test]
fn charges_add_up_and_release_on_drop() {
    let _init_guard = zebra_test::init();

    let budget = bounded(100);
    let releases = budget
        .bound
        .as_ref()
        .expect("the budget is bounded")
        .released
        .subscribe();

    let first = budget.charge(60).expect("the budget is bounded");
    assert_eq!(budget.charged_bytes(), 60);
    assert!(!budget.is_over_limit());

    let second = budget.charge(50).expect("the budget is bounded");
    assert_eq!(budget.charged_bytes(), 110);
    assert!(budget.is_over_limit(), "the charge is above the limit");

    assert!(!releases.has_changed().expect("the budget holds the sender"));
    drop(first);
    assert_eq!(budget.charged_bytes(), 50);
    assert!(!budget.is_over_limit());
    assert!(
        releases.has_changed().expect("the budget holds the sender"),
        "a release wakes waiting downloads",
    );

    drop(second);
    assert_eq!(budget.charged_bytes(), 0);

    let at_limit = budget.charge(100).expect("the budget is bounded");
    assert!(!budget.is_over_limit(), "the limit itself is not above it");
    drop(at_limit);
}

/// The downloader's charge is the state's shared block charge, computed without hashing the
/// transactions.
#[test]
fn downloaded_block_charge_is_the_shared_block_charge() {
    let _init_guard = zebra_test::init();

    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("hard-coded block vectors deserialize");
        let hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();

        assert_eq!(
            downloaded_block_charge(&block),
            u64::try_from(zs::block_retained_charge(&block, &hashes))
                .expect("usize fits in u64 on supported platforms"),
            "block at height {height}",
        );
    }
}

/// The oldest checkpoint range is the only one exempt from waiting, and only until it is
/// provably fully requested (design §4.2 step 2).
#[test]
fn oldest_range_is_fully_requested_only_when_every_height_is_covered() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let heights = |heights: &[u32]| -> BTreeMap<Height, usize> {
        heights.iter().fold(BTreeMap::new(), |mut map, height| {
            *map.entry(Height(*height)).or_default() += 1;
            map
        })
    };

    // (max checkpoint height, tip, downloaded heights, unknown heights, fully requested)
    let cases: &[(u32, Option<u32>, &[u32], usize, bool)] = &[
        // An empty state requests the genesis block without waiting.
        (10, None, &[], 0, true),
        // The range from height 1 to the checkpoint at height 5.
        (10, Some(0), &[], 0, false),
        (10, Some(0), &[1, 2, 3, 4], 0, false),
        (10, Some(0), &[1, 2, 3, 4], 1, true),
        (10, Some(0), &[], 5, true),
        (10, Some(0), &[1, 2, 3, 4, 5], 0, true),
        // Two blocks at one height cover it once.
        (10, Some(0), &[1, 1, 2, 3, 4], 0, false),
        // Heights above the range don't cover it.
        (10, Some(0), &[1, 2, 3, 4, 6, 7, 8], 0, false),
        // A range part of which is already committed.
        (10, Some(3), &[4], 0, false),
        (10, Some(3), &[4, 5], 0, true),
        // The next range starts after the tip reaches a checkpoint.
        (10, Some(5), &[1, 2, 3, 4, 5], 0, false),
        (10, Some(5), &[6, 7, 8, 9, 10], 0, true),
        // Past the last checkpoint the verifier uses, there is no range to protect, even if the
        // list has later checkpoints.
        (5, Some(5), &[], 0, true),
        (10, Some(10), &[], 0, true),
        (10, Some(u32::MAX), &[], 0, true),
    ];

    for &(max_checkpoint_height, tip, downloaded, unknown, expected) in cases {
        assert_eq!(
            oldest_range_is_fully_requested(
                &checkpoints,
                Height(max_checkpoint_height),
                tip.map(Height),
                &heights(downloaded),
                unknown,
            ),
            expected,
            "max checkpoint {max_checkpoint_height}, tip {tip:?}, downloaded {downloaded:?}, \
             unknown {unknown}",
        );
    }
}

/// The admission state counts each download until the syncer observes its result, and ignores
/// reports from downloads it forgot.
#[tokio::test]
async fn admission_counts_downloads_until_their_results_are_observed() {
    let _init_guard = zebra_test::init();

    let [first, second, third] = [1, 2, 3].map(|height| mainnet_block(height).hash());
    let mut admission = PipelineAdmission::new(bounded(1), checkpoints(), Height(10))
        .expect("a bounded budget has an admission state");
    let budget = admission.budget().clone();

    let mut first_download = admission.track(first);
    let second_download = admission.track(second);
    assert_eq!(admission.unknown_heights, 2);

    first_download.downloaded(Height(1), 10);
    assert_eq!(budget.charged_bytes(), 10);
    admission.drain();
    assert_eq!(admission.unknown_heights, 1);
    assert_eq!(admission.heights.get(&Height(1)), Some(&1));
    assert_eq!(
        admission.downloaded.get(&first),
        Some(&Some((Height(1), 10)))
    );

    // A failed download stops counting as soon as its task ends, before the syncer observes it.
    drop(second_download);
    admission.drain();
    assert_eq!(admission.unknown_heights, 0);
    assert!(admission.finished.contains(&second));
    admission.observe(&second);
    assert!(admission.finished.is_empty());

    drop(first_download);
    assert_eq!(
        budget.charged_bytes(),
        0,
        "ending the task releases its charge"
    );
    admission.observe(&first);
    assert!(admission.is_empty());

    // After a reset, a cancelled task's report doesn't touch a new download of the same hash.
    let cancelled = admission.track(third);
    admission.clear();
    let _requeued = admission.track(third);
    drop(cancelled);
    admission.drain();
    assert_eq!(admission.downloaded.get(&third), Some(&None));
    assert_eq!(admission.unknown_heights, 1);
    assert!(admission.finished.is_empty());
}

/// After the last checkpoint, a bounded pipeline only admits a download while its charge is at or
/// below the bound, defers it at the download deadline, and admits it as soon as a commit
/// releases enough bytes.
#[tokio::test(start_paused = true)]
async fn bounded_pipeline_admits_downloads_only_at_or_below_its_limit() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<_> = (1..=4).map(mainnet_block).collect();
    let charges: Vec<u64> = blocks
        .iter()
        .map(|block| downloaded_block_charge(block))
        .collect();
    // The third block takes the charge one byte over the limit.
    let limit = charges[..3].iter().sum::<u64>() - 1;

    let (mut downloads, mut network, mut verifier) = bounded_downloads(Height(0), limit);
    let budget = downloads.pipeline_budget();

    let mut verifications = Vec::new();
    for block in &blocks[..3] {
        assert!(budget.charged_bytes() <= limit);
        verifications.push(deliver(&mut downloads, &mut network, &mut verifier, block).await);
    }
    assert_eq!(budget.charged_bytes(), limit + 1);

    let waiting = blocks[3].hash();
    let started = Instant::now();
    let error = downloads
        .download_and_verify(waiting)
        .await
        .expect_err("a download above the limit waits, then defers");
    assert!(is_deferred(&error, waiting), "{error:?}");
    assert_eq!(started.elapsed(), BLOCK_DOWNLOAD_ROUND_TIMEOUT);
    network.expect_no_requests().await;
    assert_eq!(budget.charged_bytes(), limit + 1);

    let first_verification = verifications.remove(0);
    let first_hash = blocks[0].hash();
    let started = Instant::now();
    let (admitted, ()) = tokio::join!(downloads.download_and_verify(waiting), async move {
        first_verification.respond(first_hash)
    });
    admitted.expect("a commit that releases enough bytes admits the waiting download");
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(budget.charged_bytes(), charges[1] + charges[2]);
    let _pending_download = network
        .expect_request(zn::Request::BlockDownload {
            hash: waiting,
            advertiser: None,
        })
        .await;
}

/// A release by another holder of the shared bound, like the inbound block downloader, admits a
/// waiting download.
#[tokio::test(start_paused = true)]
async fn shared_release_admits_a_waiting_download() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, _verifier) = bounded_downloads(Height(0), 1);
    let inbound_charge = downloads
        .pipeline_budget()
        .charge(2)
        .expect("the pipeline is bounded");

    let hash = mainnet_block(1).hash();
    let release_delay = Duration::from_secs(1);
    let started = Instant::now();
    let (admitted, ()) = tokio::join!(downloads.download_and_verify(hash), async move {
        tokio::time::sleep(release_delay).await;
        drop(inbound_charge);
    });
    admitted.expect("the release admits the waiting download");
    assert_eq!(started.elapsed(), release_delay);
    let _pending_download = network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await;
}

/// Shrinking the ceiling rejects subsequent gossip and sync admissions until held charges drain.
#[tokio::test(start_paused = true)]
async fn runtime_shrink_applies_to_new_gossip_and_sync_admissions() {
    let _init_guard = zebra_test::init();

    let budget = bounded(100);
    let first = budget.charge(60).expect("the budget is bounded");
    let second = budget.charge(20).expect("the budget is bounded");
    budget.update_limit(50);
    assert_eq!(budget.limit(), Some(50));
    assert!(budget.is_over_limit());
    assert_eq!(
        budget
            .try_charge(1)
            .expect_err("gossip cannot charge above the shrunken ceiling"),
        OverLimit {
            charged: 80,
            limit: 50,
        }
    );

    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(Height(0));
    let mut admission = PipelineAdmission::new(budget.clone(), checkpoints(), Height(0))
        .expect("the budget is bounded");
    admission
        .admit(&tip, Instant::now())
        .await
        .expect_err("sync waits while held charges exceed the shrunken ceiling");

    drop(first);
    assert_eq!(budget.charged_bytes(), 20);
    admission
        .admit(&tip, Instant::now())
        .await
        .expect("draining below the new ceiling admits sync work");
    assert_eq!(
        budget
            .try_charge(31)
            .expect_err("gossip still checks the new ceiling"),
        OverLimit {
            charged: 20,
            limit: 50,
        }
    );
    let fitting = budget
        .try_charge(30)
        .expect("an exact fit is admitted")
        .expect("the budget is bounded");
    drop(fitting);
    drop(second);
    assert_eq!(budget.charged_bytes(), 0);
}

/// Growing the ceiling wakes a blocked sync admission even before any held charge is released.
#[tokio::test(start_paused = true)]
async fn runtime_growth_wakes_waiting_sync_admission() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, _verifier) = bounded_downloads(Height(0), 1);
    let budget = downloads.pipeline_budget();
    let held = budget.charge(2).expect("the budget is bounded");
    let updater = budget.clone();
    let growth_delay = Duration::from_secs(1);
    let hash = mainnet_block(1).hash();

    let started = Instant::now();
    let (admitted, ()) = tokio::join!(downloads.download_and_verify(hash), async move {
        tokio::time::sleep(growth_delay).await;
        updater.update_limit(2);
    });
    admitted.expect("growing the ceiling wakes the waiting download");
    assert_eq!(started.elapsed(), growth_delay);
    assert_eq!(budget.limit(), Some(2));
    assert_eq!(budget.charged_bytes(), 2, "the held charge is unchanged");

    let _pending_download = network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await;
    drop(held);
}

/// Above the bound, the downloads of the oldest checkpoint range are still admitted without
/// waiting, and the next range waits once the oldest is fully requested (design §4.2 step 2).
#[tokio::test(start_paused = true)]
async fn oldest_checkpoint_range_is_admitted_above_the_limit() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<_> = (1..=6).map(mainnet_block).collect();
    let (mut downloads, mut network, mut verifier) = bounded_downloads(Height(10), 1);
    let budget = downloads.pipeline_budget();

    let started = Instant::now();
    let mut verifications = Vec::new();
    for block in &blocks[..5] {
        verifications.push(deliver(&mut downloads, &mut network, &mut verifier, block).await);
    }
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "the oldest range never waits"
    );
    assert_eq!(
        budget.charged_bytes(),
        blocks[..5]
            .iter()
            .map(|block| downloaded_block_charge(block))
            .sum::<u64>(),
    );
    assert!(budget.is_over_limit(), "the exempt range is over-admitted");

    let next_range = blocks[5].hash();
    let error = downloads
        .download_and_verify(next_range)
        .await
        .expect_err("the next range waits once the oldest is fully requested");
    assert!(is_deferred(&error, next_range), "{error:?}");
    network.expect_no_requests().await;
}

/// A download in the oldest range that a stuck peer never serves can't block that range: when
/// its download round fails, the next block is admitted, and re-obtaining it never waits
/// (design §4.2 step 2, the liveness rule).
#[tokio::test(start_paused = true)]
async fn stuck_peer_cannot_block_the_oldest_range() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<_> = (1..=6).map(mainnet_block).collect();
    let stuck = blocks[2].hash();
    let (mut downloads, mut network, mut verifier) = bounded_downloads(Height(10), 1);

    let mut verifications = Vec::new();
    for block in &blocks[..2] {
        verifications.push(deliver(&mut downloads, &mut network, &mut verifier, block).await);
    }
    downloads
        .download_and_verify(stuck)
        .await
        .expect("the oldest range is admitted");
    let stuck_download = network
        .expect_request(zn::Request::BlockDownload {
            hash: stuck,
            advertiser: None,
        })
        .await;
    for block in &blocks[3..5] {
        verifications.push(deliver(&mut downloads, &mut network, &mut verifier, block).await);
    }

    // Start waiting after the stuck download, so its round ends first.
    let head_start = Duration::from_secs(1);
    tokio::time::advance(head_start).await;

    let next = blocks[5].hash();
    let started = Instant::now();
    downloads.download_and_verify(next).await.expect(
        "a failed download leaves the oldest range incomplete, so the next block is admitted",
    );
    assert_eq!(started.elapsed(), BLOCK_DOWNLOAD_ROUND_TIMEOUT - head_start);
    let _next_download = network
        .expect_request(zn::Request::BlockDownload {
            hash: next,
            advertiser: None,
        })
        .await;
    drop(stuck_download);

    let failure = downloads
        .next()
        .await
        .expect("the stuck download has a result")
        .expect_err("the stuck download timed out");
    assert!(
        matches!(failure, BlockDownloadVerifyError::DownloadTimedOut { hash } if hash == stuck),
        "{failure:?}",
    );

    // The range is fully requested again, but the syncer's re-obtain never waits.
    assert!(downloads.pipeline_budget().is_over_limit());
    let started = Instant::now();
    downloads
        .download_and_verify_exempt(stuck)
        .await
        .expect("re-obtained blocks never wait");
    assert_eq!(started.elapsed(), Duration::ZERO);
    let _reobtained_download = network
        .expect_request(zn::Request::BlockDownload {
            hash: stuck,
            advertiser: None,
        })
        .await;
}

/// Every outcome of a download releases its charge and forgets it: a commit, a rejection, a
/// failed download, and a cancellation.
#[tokio::test(start_paused = true)]
async fn every_outcome_releases_the_charge_and_forgets_the_download() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<_> = (1..=4).map(mainnet_block).collect();
    let (mut downloads, mut network, mut verifier) = bounded_downloads(Height(10), u64::MAX);
    let budget = downloads.pipeline_budget();

    deliver(&mut downloads, &mut network, &mut verifier, &blocks[0])
        .await
        .respond(blocks[0].hash());
    deliver(&mut downloads, &mut network, &mut verifier, &blocks[1])
        .await
        .respond_error("rejected".into());

    let missing = blocks[2].hash();
    downloads
        .download_and_verify(missing)
        .await
        .expect("the download is admitted");
    network
        .expect_request(zn::Request::BlockDownload {
            hash: missing,
            advertiser: None,
        })
        .await
        .respond(Err::<zn::Response, _>(zn::BoxError::from("not found")));

    let mut verified = 0;
    for _ in 0..3 {
        if downloads
            .next()
            .await
            .expect("each download has a result")
            .is_ok()
        {
            verified += 1;
        }
    }
    assert_eq!(verified, 1, "only the first block commits");
    assert_eq!(budget.charged_bytes(), 0);
    assert!(downloads
        .admission()
        .expect("the pipeline is bounded")
        .is_empty());

    let _verification = deliver(&mut downloads, &mut network, &mut verifier, &blocks[3]).await;
    assert_eq!(budget.charged_bytes(), downloaded_block_charge(&blocks[3]));
    downloads.cancel_all();
    tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, async {
        while budget.charged_bytes() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a cancelled download releases its charge");
    assert!(downloads
        .admission()
        .expect("the pipeline is bounded")
        .is_empty());
}

/// The syncer keeps the hashes a full pipeline defers, in order, and re-obtains missing blocks
/// without waiting for the bound (design §4.2 step 2).
#[tokio::test(start_paused = true)]
async fn syncer_keeps_deferred_hashes_and_reobtains_without_waiting() {
    let _init_guard = zebra_test::init();

    let config = ZebradConfig::default();
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(Height(10));
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let mut network: MockNetwork = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let verifier: MockVerifier = MockService::build().for_unit_tests();
    let state: MockService<zs::Request, zs::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let read_state: MockReadState = MockService::build().for_unit_tests();

    let (mut chain_sync, _status) = ChainSync::new_with_limits(
        &config,
        Height(10),
        network.clone(),
        verifier,
        state,
        read_state,
        tip,
        misbehavior_tx,
        checkpoints(),
        SyncMemoryLimits {
            pipeline_bytes: Some(1),
        },
    );
    let budget = chain_sync.pipeline_budget();
    assert_eq!(budget.limit(), Some(1));
    let _held = budget.charge(2).expect("the syncer's pipeline is bounded");

    let hashes: IndexSet<block::Hash> =
        (1..=3).map(|height| mainnet_block(height).hash()).collect();
    let kept = chain_sync
        .request_blocks(hashes.clone())
        .await
        .expect("a deferred batch is not an error");
    assert_eq!(kept, hashes, "deferred hashes are kept, in order");
    network.expect_no_requests().await;

    let missing = hashes[0];
    chain_sync.reobtain_hashes.insert(missing);
    let started = Instant::now();
    chain_sync.reobtain_missing_blocks().await;
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "re-obtained blocks never wait"
    );
    assert!(chain_sync.reobtain_hashes.is_empty());
    let _reobtained_download = network
        .expect_request(zn::Request::BlockDownload {
            hash: missing,
            advertiser: None,
        })
        .await;
}

/// Returns a unique non-null hash for a synthetic checkpoint or downloaded height.
fn synthetic_hash(height: u32) -> block::Hash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    bytes[4] = 1;
    block::Hash(bytes)
}

/// A full 400-block range of 2 MB charges is admitted above a 32 MiB bound, then drains to zero.
/// The charges are simulated so this test does not retain 800 MB of actual blocks.
#[tokio::test(start_paused = true)]
async fn full_size_oldest_checkpoint_range_is_admitted_and_drains() {
    let _init_guard = zebra_test::init();

    let range_len = u32::try_from(zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP)
        .expect("the checkpoint height gap fits in a block height");
    let max_checkpoint_height = Height(range_len * 2);
    let checkpoints = Arc::new(
        CheckpointList::from_list(
            [0, range_len, range_len * 2].map(|height| (Height(height), synthetic_hash(height))),
        )
        .expect("synthetic checkpoints start at genesis and have unique entries"),
    );
    let budget = bounded(zebra_consensus::MAX_CHECKPOINT_BYTE_COUNT);
    let mut admission =
        PipelineAdmission::new(budget.clone(), checkpoints.clone(), max_checkpoint_height)
            .expect("a bounded budget has an admission state");
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(Height(0));

    let started = Instant::now();
    let mut downloads = Vec::new();
    for height in 1..=range_len {
        admission
            .admit(&tip, Instant::now())
            .await
            .expect("the oldest checkpoint range is always admitted");
        let mut download = admission.track(synthetic_hash(height));
        download.downloaded(Height(height), MAX_BLOCK_BYTES);
        downloads.push(download);
    }
    admission.drain();

    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(
        budget.charged_bytes(),
        MAX_BLOCK_BYTES * u64::from(range_len)
    );
    assert!(budget.is_over_limit());
    assert!(oldest_range_is_fully_requested(
        &checkpoints,
        max_checkpoint_height,
        Some(Height(0)),
        &admission.heights,
        admission.unknown_heights,
    ));

    admission
        .admit(&tip, Instant::now())
        .await
        .expect_err("the next range waits while the completed oldest range is over the bound");

    for (height, download) in (1..=range_len).zip(downloads) {
        drop(download);
        admission.observe(&synthetic_hash(height));
    }
    assert_eq!(budget.charged_bytes(), 0);
    assert!(admission.is_empty());

    tip_sender.send_best_tip_height(Height(range_len));
    admission
        .admit(&tip, Instant::now())
        .await
        .expect("draining the oldest range admits the next range");
}

/// The retained-charge ratio stays below the documented adversarial maximum for the three block
/// shapes that maximize decoded transaction structures relative to their wire bytes.
#[test]
fn adversarial_block_charge_ratios_stay_below_the_documented_maximum() {
    let _init_guard = zebra_test::init();

    // Many minimal transactions are the dominant shape at about 36 times their wire bytes with the
    // current decoded `Transaction` representation. Leave headroom for representation changes.
    const MAX_ADVERSARIAL_CHARGE_RATIO: u64 = 40;

    let shapes = [
        ("many transactions", large_multi_transaction_block()),
        ("many inputs", large_single_transaction_block_many_inputs()),
        (
            "many outputs",
            large_single_transaction_block_many_outputs(),
        ),
    ];

    for (shape, block) in shapes {
        let wire = u64::try_from(block.zcash_serialized_size())
            .expect("serialized block sizes fit in u64");
        assert!((MAX_BLOCK_BYTES - 100..=MAX_BLOCK_BYTES).contains(&wire));

        let charge = downloaded_block_charge(&block);
        let ratio_thousandths = charge.saturating_mul(1000) / wire;
        tracing::info!(
            shape,
            wire,
            charge,
            ratio_thousandths,
            "adversarial retained-charge ratio"
        );
        assert!(
            charge <= wire.saturating_mul(MAX_ADVERSARIAL_CHARGE_RATIO),
            "{shape} charge {charge} exceeds {MAX_ADVERSARIAL_CHARGE_RATIO} times {wire} wire bytes",
        );
    }
}

/// Refusing an addition that would overflow preserves the exact charge for every guard's release.
#[test]
fn charge_overflow_never_corrupts_accounting() {
    let _init_guard = zebra_test::init();

    let budget = bounded(u64::MAX);
    let first = budget
        .try_charge(u64::MAX)
        .expect("the first charge fits exactly")
        .expect("the budget is bounded");
    let overflow = match budget.try_charge(1) {
        Err(error) => error,
        Ok(unexpected_charge) => {
            std::mem::forget(unexpected_charge);
            std::mem::forget(first);
            panic!("an overflowing charge must not return a guard");
        }
    };
    assert_eq!(
        overflow,
        OverLimit {
            charged: u64::MAX,
            limit: u64::MAX,
        }
    );
    assert_eq!(budget.charged_bytes(), u64::MAX);
    drop(first);
    assert_eq!(budget.charged_bytes(), 0);

    let first = budget.charge(u64::MAX).expect("the budget is bounded");
    let overflow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| budget.charge(1)));
    if let Ok(unexpected_charge) = overflow {
        std::mem::forget(unexpected_charge);
        std::mem::forget(first);
        panic!("an overflowing charge must not return a guard");
    }
    assert_eq!(budget.charged_bytes(), u64::MAX);
    drop(first);
    assert_eq!(budget.charged_bytes(), 0);
}

/// Returns a block just below the wire-size limit with as many small transactions as possible.
fn large_multi_transaction_block() -> Block {
    let transaction = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..])
        .expect("the hard-coded transaction deserializes");
    let transaction_bytes = transaction
        .zcash_serialize_to_vec()
        .expect("the hard-coded transaction serializes");
    let (header, header_bytes) = test_block_header();
    let max_bytes = usize::try_from(MAX_BLOCK_BYTES).expect("the block size limit fits in usize");
    let transaction_count = (max_bytes - header_bytes.len() - 4) / transaction_bytes.len();
    let transactions = std::iter::repeat_n(Arc::new(transaction), transaction_count).collect();

    Block {
        header: header.into(),
        transactions,
    }
}

/// Returns a block just below the wire-size limit with as many transparent inputs as possible.
fn large_single_transaction_block_many_inputs() -> Block {
    let input = transparent::Input::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..])
        .expect("the hard-coded input deserializes");
    let input_bytes = input
        .zcash_serialize_to_vec()
        .expect("the hard-coded input serializes");
    let output = transparent::Output::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..])
        .expect("the hard-coded output deserializes");
    let output_bytes = output
        .zcash_serialize_to_vec()
        .expect("the hard-coded output serializes");
    let (header, header_bytes) = test_block_header();
    let lock_time =
        LockTime::Time(DateTime::from_timestamp(61, 0).expect("the hard-coded lock time is valid"));
    let lock_time_bytes = lock_time
        .zcash_serialize_to_vec()
        .expect("the hard-coded lock time serializes");
    let max_bytes = usize::try_from(MAX_BLOCK_BYTES).expect("the block size limit fits in usize");
    let input_count = (max_bytes
        - header_bytes.len()
        - 1
        - 4
        - lock_time_bytes.len()
        - 4
        - 1
        - output_bytes.len())
        / input_bytes.len();
    let transaction = Transaction::test_v1(
        std::iter::repeat_n(input, input_count).collect(),
        vec![output],
        lock_time,
    );

    Block {
        header: header.into(),
        transactions: vec![Arc::new(transaction)],
    }
}

/// Returns a block just below the wire-size limit with as many transparent outputs as possible.
fn large_single_transaction_block_many_outputs() -> Block {
    let input = transparent::Input::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..])
        .expect("the hard-coded input deserializes");
    let input_bytes = input
        .zcash_serialize_to_vec()
        .expect("the hard-coded input serializes");
    let output = transparent::Output::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..])
        .expect("the hard-coded output deserializes");
    let output_bytes = output
        .zcash_serialize_to_vec()
        .expect("the hard-coded output serializes");
    let (header, header_bytes) = test_block_header();
    let lock_time =
        LockTime::Time(DateTime::from_timestamp(61, 0).expect("the hard-coded lock time is valid"));
    let lock_time_bytes = lock_time
        .zcash_serialize_to_vec()
        .expect("the hard-coded lock time serializes");
    let max_bytes = usize::try_from(MAX_BLOCK_BYTES).expect("the block size limit fits in usize");
    let output_count = (max_bytes
        - header_bytes.len()
        - 1
        - 4
        - lock_time_bytes.len()
        - 1
        - input_bytes.len()
        - 4)
        / output_bytes.len();
    let transaction = Transaction::test_v1(
        vec![input],
        std::iter::repeat_n(output, output_count).collect(),
        lock_time,
    );

    Block {
        header: header.into(),
        transactions: vec![Arc::new(transaction)],
    }
}

/// Returns a test header and its canonical serialized bytes.
fn test_block_header() -> (block::Header, Vec<u8>) {
    let header = block::Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..])
        .expect("the hard-coded header deserializes");
    let bytes = header
        .zcash_serialize_to_vec()
        .expect("the hard-coded header serializes");
    (header, bytes)
}
