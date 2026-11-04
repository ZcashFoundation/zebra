//! Single-hop parent recovery through the ordinary gossip download and commit queue.

use std::collections::HashSet;

use tokio::time::timeout;
use tower::{timeout::Timeout, ServiceExt};

use super::*;
use crate::components::sync::BLOCK_VERIFY_TIMEOUT;
use zebra_chain::{block::Hash, parameters::Network};
use zebra_test::mock_service::ResponseSender;

use super::super::{BoxError, MAX_PARENT_RECOVERIES};

type TestDownloads = Downloads<MockNetwork, Timeout<MockVerifier>, MockState, MockChainTip>;
type NetworkResponse = ResponseSender<zn::Request, zn::Response, BoxError>;

fn block(height: u32) -> Arc<Block> {
    zebra_test::vectors::MAINNET_BLOCKS[&height]
        .zcash_deserialize_into()
        .unwrap()
}

fn peer(index: u8) -> PeerSocketAddr {
    ([127, 0, 0, index], 8233).into()
}

fn setup(limit: usize) -> (TestDownloads, MockNetwork, MockVerifier, MockState) {
    let (_, network, verifier, state) = mock_downloads(Height(10));
    let (tip, sender) = MockChainTip::new();
    sender.send_best_tip_height(Height(10));
    let downloads = Downloads::new(
        limit,
        network.clone(),
        Timeout::new(verifier.clone(), BLOCK_VERIFY_TIMEOUT),
        state.clone(),
        tip,
        Network::Mainnet,
    );
    (downloads, network, verifier, state)
}

async fn request(
    network: &mut MockNetwork,
    state: &mut MockState,
    hash: Hash,
    advertiser: PeerSocketAddr,
) -> NetworkResponse {
    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
}

async fn child(
    downloads: &mut TestDownloads,
    network: &mut MockNetwork,
    state: &mut MockState,
    block: Arc<Block>,
    advertiser: PeerSocketAddr,
    source: PeerSocketAddr,
) {
    assert!(matches!(
        downloads.download_and_verify(block.hash(), Some(advertiser)),
        DownloadAction::AddedToQueue,
    ));
    request(network, state, block.hash(), advertiser)
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(source),
        ))]));
    state
        .expect_request(zs::Request::KnownBlock(block.header.previous_block_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
}

/// The real state queue commits an out-of-order child once its single recovered parent arrives.
#[tokio::test(flavor = "multi_thread")]
async fn out_of_order_child_recovers_parent_once() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(30), async {
        let (state, _read, tip, _change) =
            zs::init(zs::Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
        let state = tower::buffer::Buffer::new(state, 4);
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(genesis().into()))
            .await
            .unwrap();
        // Historical vectors stand in for semantic verification; exercise the real commit queue.
        let verifier = state
            .clone()
            .map_request(|request: zebra_consensus::Request| match request {
                zebra_consensus::Request::Commit(block)
                | zebra_consensus::Request::CommitEarlyRelay(block) => {
                    zs::Request::CommitCheckpointVerifiedBlock(block.into())
                }
                _ => panic!("the downloader only submits commit requests"),
            })
            .map_response(|response| match response {
                zs::Response::Committed(hash) => hash,
                _ => unreachable!("a checkpoint commit returns its hash"),
            });
        let mut network: MockNetwork = MockService::build()
            .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
            .for_unit_tests();
        let mut downloads = Downloads::new(
            4,
            network.clone(),
            Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT),
            state,
            tip,
            Network::Mainnet,
        );
        let child = block_2();
        let parent = block_1();
        assert!(matches!(
            downloads.download_and_verify(child.hash(), Some(peer(1))),
            DownloadAction::AddedToQueue,
        ));
        network
            .expect_request(zn::Request::BlockDownload {
                hash: child.hash(),
                advertiser: Some(peer(1)),
            })
            .await
            .respond(zn::Response::EarlyBlocks(vec![Available((
                child.clone(),
                Some(peer(2)),
            ))]));
        // Recovery prefers the actual source, not the peer whose inventory admitted the child.
        network
            .expect_request(zn::Request::BlockDownload {
                hash: parent.hash(),
                advertiser: Some(peer(2)),
            })
            .await
            .respond(zn::Response::Blocks(vec![Available((
                parent.clone(),
                Some(peer(2)),
            ))]));
        let mut committed = HashSet::new();
        for _ in 0..2 {
            committed.insert(downloads.next().await.unwrap().unwrap());
        }
        assert_eq!(committed, HashSet::from([child.hash(), parent.hash()]));
        network.expect_no_requests().await;
    })
    .await
    .unwrap();
}

/// A slow ordinary parent download owns the hash; the child must not open a second request.
#[tokio::test(start_paused = true)]
async fn in_flight_parent_download_is_reused() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, mut state) = setup(4);
    let parent = block_1();
    let child_block = block_2();
    assert!(matches!(
        downloads.download_and_verify(parent.hash(), Some(peer(1))),
        DownloadAction::AddedToQueue
    ));
    let parent_response = request(&mut network, &mut state, parent.hash(), peer(1)).await;
    child(
        &mut downloads,
        &mut network,
        &mut state,
        child_block.clone(),
        peer(2),
        peer(2),
    )
    .await;
    let child_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(child_block.clone()))
        .await;
    assert!(downloads.spawner.admission().parent_recoveries.is_empty());
    network.expect_no_requests().await;
    parent_response.respond(zn::Response::Blocks(vec![Available((
        parent.clone(),
        Some(peer(1)),
    ))]));
    state
        .expect_request(zs::Request::KnownBlock(parent.header.previous_block_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::Finalized)));
    verifier
        .expect_request(zebra_consensus::Request::Commit(parent.clone()))
        .await
        .respond(parent.hash());
    child_commit.respond(child_block.hash());
    let mut committed = HashSet::new();
    for _ in 0..2 {
        committed.insert(downloads.next().await.unwrap().unwrap());
    }
    assert_eq!(
        committed,
        HashSet::from([parent.hash(), child_block.hash()])
    );
}

/// A recovered parent with another missing ancestor must not start a second recovery hop.
#[tokio::test(start_paused = true)]
async fn recovered_parent_does_not_recover_grandparent() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, mut state) = setup(4);
    let child_block = block(3);
    let parent = block_2();
    child(
        &mut downloads,
        &mut network,
        &mut state,
        child_block.clone(),
        peer(1),
        peer(1),
    )
    .await;
    let child_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(child_block.clone()))
        .await;
    request(&mut network, &mut state, parent.hash(), peer(1))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            parent.clone(),
            Some(peer(1)),
        ))]));
    let parent_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(parent))
        .await;
    state.expect_no_requests().await;
    network.expect_no_requests().await;
    assert_eq!(downloads.spawner.admission().parent_recoveries.len(), 1);
    parent_commit.respond_error("unknown grandparent".into());
    child_commit.respond_error("unknown parent".into());
    for _ in 0..2 {
        assert!(downloads.next().await.unwrap().is_err());
    }
}

/// Recovery obeys both the global recovery cap and the existing total download limit.
#[tokio::test(start_paused = true)]
async fn parent_recoveries_are_bounded() {
    let _init_guard = zebra_test::init();
    for limit in [2, 10] {
        let (mut downloads, mut network, mut verifier, mut state) = setup(limit);
        let count = if limit == 2 { 1 } else { MAX_PARENT_RECOVERIES };
        let mut held_downloads = Vec::new();
        let mut held_commits = Vec::new();
        for index in 0..count {
            let height = u32::try_from(2 * (index + 1)).unwrap();
            let source = peer(u8::try_from(index + 1).unwrap());
            let child_block = block(height);
            child(
                &mut downloads,
                &mut network,
                &mut state,
                child_block.clone(),
                source,
                source,
            )
            .await;
            held_commits.push(
                verifier
                    .expect_request(zebra_consensus::Request::Commit(child_block))
                    .await,
            );
            held_downloads
                .push(request(&mut network, &mut state, block(height - 1).hash(), source).await);
        }
        assert_eq!(downloads.spawner.admission().parent_recoveries.len(), count);
        if limit == 2 {
            assert!(matches!(
                downloads.download_and_verify(block(10).hash(), Some(peer(10))),
                DownloadAction::FullQueue
            ));
        } else {
            child(
                &mut downloads,
                &mut network,
                &mut state,
                block(10),
                peer(10),
                peer(10),
            )
            .await;
            held_commits.push(
                verifier
                    .expect_request(zebra_consensus::Request::Commit(block(10)))
                    .await,
            );
            assert_eq!(
                downloads.spawner.admission().parent_recoveries.len(),
                MAX_PARENT_RECOVERIES
            );
            state.expect_no_requests().await;
            network.expect_no_requests().await;
        }
        // Dropping the stream cancels both ordinary and recovery tasks, including uncollected ones.
        drop(downloads);
        tokio::task::yield_now().await;
        drop(held_downloads);
        drop(held_commits);
    }
}

/// Distinct children served by the same IP cannot each consume a recovery slot.
#[tokio::test(start_paused = true)]
async fn parent_recoveries_are_bounded_per_source() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, mut state) = setup(10);
    child(
        &mut downloads,
        &mut network,
        &mut state,
        block(2),
        peer(2),
        peer(9),
    )
    .await;
    let first_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(block(2)))
        .await;
    let parent = request(&mut network, &mut state, block(1).hash(), peer(9)).await;
    assert!(matches!(
        downloads.download_and_verify(block(1).hash(), Some(peer(3))),
        DownloadAction::AlreadyQueued,
    ));
    child(
        &mut downloads,
        &mut network,
        &mut state,
        block(4),
        peer(4),
        peer(9),
    )
    .await;
    let second_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(block(4)))
        .await;
    assert_eq!(downloads.spawner.admission().parent_recoveries.len(), 1);
    network.expect_no_requests().await;
    state.expect_no_requests().await;
    let recovery_task = downloads.spawned_receiver.recv().await.unwrap();
    drop(downloads);
    let (error, _) = recovery_task.await.unwrap().unwrap_err();
    assert_eq!(error.to_string(), "canceled");
    drop((parent, first_commit, second_commit));
}

/// Failed or invalid parents release admission immediately; waiting children retain their deadline.
#[tokio::test(start_paused = true)]
async fn failed_parent_does_not_wedge_child_slot() {
    let _init_guard = zebra_test::init();
    for invalid in [false, true] {
        let (mut downloads, mut network, mut verifier, mut state) = setup(2);
        let child_block = block_2();
        let parent = block_1();
        child(
            &mut downloads,
            &mut network,
            &mut state,
            child_block.clone(),
            peer(1),
            peer(1),
        )
        .await;
        let child_commit = verifier
            .expect_request(zebra_consensus::Request::Commit(child_block))
            .await;
        let parent_response = request(&mut network, &mut state, parent.hash(), peer(1)).await;
        if invalid {
            parent_response.respond(zn::Response::Blocks(vec![Available((
                parent.clone(),
                Some(peer(1)),
            ))]));
            verifier
                .expect_request(zebra_consensus::Request::Commit(parent))
                .await
                .respond_error("invalid parent".into());
        } else {
            parent_response.respond_error("parent unavailable".into());
        }
        // Await the recovery handle directly: admission must clear before the stream is polled.
        let parent_task = downloads.spawned_receiver.recv().await.unwrap();
        assert!(parent_task.await.unwrap().is_err());
        assert!(downloads.spawner.admission().parent_recoveries.is_empty());
        let (error, source) = downloads.next().await.unwrap().unwrap_err();
        assert!(error.is::<tower::timeout::error::Elapsed>());
        assert_eq!(source, Some(peer(1)));
        assert!(downloads.spawner.admission().cancel_handles.is_empty());
        network.expect_no_requests().await;
        assert!(matches!(
            downloads.download_and_verify(block(3).hash(), Some(peer(1))),
            DownloadAction::AddedToQueue,
        ));
        drop(child_commit);
    }
}

/// A child without valid proof of work never fetches its parent, and releases its recovery slot.
#[tokio::test(start_paused = true)]
async fn invalid_child_does_not_recover_parent() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, mut state) = setup(4);
    let mut invalid = (*block_2()).clone();
    Arc::make_mut(&mut invalid.header).nonce = [0; 32].into();
    let invalid = Arc::new(invalid);
    child(
        &mut downloads,
        &mut network,
        &mut state,
        invalid.clone(),
        peer(1),
        peer(1),
    )
    .await;
    let child_commit = verifier
        .expect_request(zebra_consensus::Request::Commit(invalid))
        .await;
    let recovery_task = downloads.spawned_receiver.recv().await.unwrap();
    let (error, source) = recovery_task.await.unwrap().unwrap_err();
    assert_eq!(
        error.to_string(),
        "child of missing parent has invalid proof of work"
    );
    assert_eq!(source, None);
    assert!(downloads.spawner.admission().parent_recoveries.is_empty());
    child_commit.respond_error("invalid proof of work".into());
    assert!(downloads.next().await.unwrap().is_err());
    state.expect_no_requests().await;
    network.expect_no_requests().await;
}

/// Proof-of-work screens run only in admitted recoveries, so children never wait for them.
#[test]
fn parent_recovery_admission_bounds_screens() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let _init_guard = zebra_test::init();
        // Occupy the only blocking worker, so any child awaiting its own screen stalls.
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(120)).unwrap();
        });
        ready.await.unwrap();
        let (mut downloads, mut network, mut verifier, mut state) = setup(10);
        let mut held_commits = Vec::new();
        for index in 0..=MAX_PARENT_RECOVERIES {
            let height = u32::try_from(2 * (index + 1)).unwrap();
            let source = peer(u8::try_from(index + 1).unwrap());
            child(
                &mut downloads,
                &mut network,
                &mut state,
                block(height),
                source,
                source,
            )
            .await;
            held_commits.push(
                verifier
                    .expect_request(zebra_consensus::Request::Commit(block(height)))
                    .await,
            );
        }
        assert_eq!(
            downloads.spawner.admission().parent_recoveries.len(),
            MAX_PARENT_RECOVERIES
        );
        // Dropping the stream cancels the queued screens before they make any requests.
        drop(downloads);
        release.send(()).unwrap();
        blocker.await.unwrap();
        drop(held_commits);
    });
}
