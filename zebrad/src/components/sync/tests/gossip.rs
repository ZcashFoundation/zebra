//! Tests for block and transaction gossip.
//!
//! These tests cover mined-block coalescing while service readiness waits for eligible peers,
//! including backpressure (<https://github.com/ZcashFoundation/zebra/issues/11475>).

use std::{
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};

use chrono::Utc;
use futures::future;
use tokio::sync::{mpsc, oneshot};
use tower::Service;

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
};
use zebra_network as zn;
use zebra_rpc::SubmitBlockChannel;
use zebra_state::{ChainTipBlock, ChainTipSender};

use crate::{
    components::sync::{
        gossip_best_tip_block_hashes, BlockGossipError, SyncStatus, TIPS_RESPONSE_TIMEOUT,
    },
    BoxError,
};

/// More mined block announcements than the submit block channel can hold.
const MORE_THAN_CHANNEL_CAPACITY: u32 = 10_500;

/// The readiness of a [`GatedPeerSet`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Readiness {
    Pending,
    Ready,
    Failed,
}

#[derive(Debug)]
struct GateState {
    readiness: Readiness,
    wakers: Vec<Waker>,
    response: Option<oneshot::Receiver<Result<zn::Response, BoxError>>>,
}

/// A fake peer set whose `poll_ready()` is controlled by the test,
/// and which forwards all requests it receives to the test.
#[derive(Clone, Debug)]
struct GatedPeerSet {
    state: Arc<Mutex<GateState>>,
    requests: mpsc::Sender<zn::Request>,
    waiting: Arc<tokio::sync::Notify>,
}

impl GatedPeerSet {
    fn new() -> (Self, mpsc::Receiver<zn::Request>) {
        let (requests, request_receiver) =
            mpsc::channel(usize::try_from(MORE_THAN_CHANNEL_CAPACITY).unwrap());
        let peer_set = GatedPeerSet {
            state: Arc::new(Mutex::new(GateState {
                readiness: Readiness::Pending,
                wakers: Vec::new(),
                response: None,
            })),
            requests,
            waiting: Default::default(),
        };

        (peer_set, request_receiver)
    }

    fn set_readiness(&self, readiness: Readiness) {
        let mut state = self.state.lock().expect("lock is not poisoned");
        state.readiness = readiness;
        state.wakers.drain(..).for_each(Waker::wake);
    }

    fn hold_next_response(&self) -> oneshot::Sender<Result<zn::Response, BoxError>> {
        let (sender, receiver) = oneshot::channel();
        self.state.lock().expect("lock is not poisoned").response = Some(receiver);
        sender
    }
}

impl Service<zn::Request> for GatedPeerSet {
    type Response = zn::Response;
    type Error = BoxError;
    type Future = future::BoxFuture<'static, Result<zn::Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let mut state = self.state.lock().expect("lock is not poisoned");
        match state.readiness {
            Readiness::Pending => {
                state.wakers.push(cx.waker().clone());
                self.waiting.notify_one();
                Poll::Pending
            }
            Readiness::Ready => Poll::Ready(Ok(())),
            Readiness::Failed => Poll::Ready(Err("permanent peer set failure".into())),
        }
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        let response = self
            .state
            .lock()
            .expect("lock is not poisoned")
            .response
            .take();
        self.requests
            .try_send(request)
            .expect("the bounded test request log has capacity");
        Box::pin(async move {
            match response {
                Some(response) => response
                    .await
                    .unwrap_or_else(|_| Err("test broadcast response was abandoned".into())),
                None => Ok(zn::Response::Nil),
            }
        })
    }
}

fn mined_block(n: u32) -> (block::Hash, Height) {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&n.to_le_bytes());
    (block::Hash(hash), Height(n))
}

fn set_tip(sender: &mut ChainTipSender, n: u32) -> block::Hash {
    let (hash, height) = mined_block(n);
    sender.set_finalized_tip(ChainTipBlock {
        hash,
        height,
        time: Utc::now(),
        transactions: Vec::new(),
        transaction_hashes: Arc::new([]),
        previous_block_hash: mined_block(n.saturating_sub(1)).0,
    });
    hash
}

/// A second tip one second after the first advertisement does not wait for a gossip interval.
#[tokio::test(start_paused = true)]
async fn gossip_consecutive_tips_without_batch_delay() {
    let _init_guard = zebra_test::init();
    let (mut tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    recent_syncs.push_extend_tips_length(0);
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set,
        None,
        Default::default(),
    ));

    for n in 1..=2 {
        let hash = set_tip(&mut tip_sender, n);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(10), requests.recv())
                .await
                .unwrap(),
            Some(zn::Request::AdvertiseBlock(hash, None)),
        );
        if n == 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    gossip.abort();
}

/// In-flight mined broadcasts suppress duplicate tip advertisements only on success.
#[tokio::test(start_paused = true)]
async fn gossip_mined_broadcast_outcome_controls_committed_tip_fallback() {
    let _init_guard = zebra_test::init();
    for outcome in [Some(true), Some(false), None] {
        let (mut tip_sender, _latest_tip, tip_change) =
            ChainTipSender::new(None, &Network::Mainnet);
        let (sync_status, mut recent_syncs) = SyncStatus::new();
        recent_syncs.push_extend_tips_length(0);
        let (peer_set, mut requests) = GatedPeerSet::new();
        peer_set.set_readiness(Readiness::Ready);
        let mut response = peer_set.hold_next_response();
        let channel = SubmitBlockChannel::new();
        let sender = channel.sender();

        // Both inputs are ready together: the mined submission must win over the tip fallback.
        let hash = set_tip(&mut tip_sender, 1);
        sender.try_send(mined_block(1)).unwrap();
        let gossip = tokio::spawn(gossip_best_tip_block_hashes(
            sync_status,
            tip_change,
            peer_set,
            Some(channel.receiver()),
            Default::default(),
        ));
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(10), requests.recv())
                .await
                .unwrap(),
            Some(zn::Request::AdvertiseBlockToAll(hash)),
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            requests.try_recv().is_err(),
            "in-flight broadcasts suppress the tip fallback"
        );
        match outcome {
            Some(true) => response.send(Ok(zn::Response::Nil)).unwrap(),
            Some(false) => response.send(Err("broadcast failed".into())).unwrap(),
            None => {
                // Keep the response sender alive until the network timeout cancels its receiver.
                tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, response.closed())
                    .await
                    .expect("the production network timeout cancels the pending response");
                drop(response);
            }
        }

        if outcome == Some(true) {
            // Another submission must not reset the successful best-tip mark.
            sender.try_send(mined_block(2)).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(10), requests.recv())
                    .await
                    .unwrap(),
                Some(zn::Request::AdvertiseBlockToAll(mined_block(2).0)),
            );
        } else {
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(10), requests.recv())
                    .await
                    .unwrap(),
                Some(zn::Request::AdvertiseBlock(hash, None)),
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            requests.try_recv().is_err(),
            "the committed tip is advertised only once"
        );
        gossip.abort();
    }
}

/// Transaction announcements retain batching at the configured gossip interval.
#[tokio::test(start_paused = true)]
async fn transaction_gossip_batches_at_configured_interval() {
    use crate::components::mempool::{
        gossip::TRANSACTION_GOSSIP_DELAY, gossip_mempool_transaction_id,
    };
    use std::collections::HashSet;
    use zebra_chain::transaction::{Hash, UnminedTxId};
    use zebra_node_services::mempool::MempoolChange;

    let _init_guard = zebra_test::init();
    let (sender, receiver) = tokio::sync::broadcast::channel(2);
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let gossip = tokio::spawn(gossip_mempool_transaction_id(receiver, peer_set));
    let first: HashSet<_> = [UnminedTxId::Legacy(Hash([1; 32]))].into();
    sender.send(MempoolChange::added(first.clone())).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(10), requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseTransactionIds(first, None)),
    );

    let second: HashSet<_> = [UnminedTxId::Legacy(Hash([2; 32]))].into();
    sender.send(MempoolChange::added(second.clone())).unwrap();
    tokio::time::sleep(TRANSACTION_GOSSIP_DELAY / 2).await;
    assert!(
        requests.try_recv().is_err(),
        "transaction gossip must batch announcements"
    );
    tokio::time::sleep(TRANSACTION_GOSSIP_DELAY / 2).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(10), requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseTransactionIds(second, None)),
    );
    gossip.abort();
}

/// Backpressure must retain committed inventory rather than silently coalesce unrelated forks.
#[tokio::test(start_paused = true)]
async fn gossip_backpressures_committed_blocks_without_ready_peers() {
    let _init_guard = zebra_test::init();
    let (_tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();
    let channel = SubmitBlockChannel::new();
    let sender = channel.sender();
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        Some(channel.receiver()),
        Default::default(),
    ));

    let mut accepted = std::collections::HashSet::new();
    let mut backpressured = false;
    for n in 0..MORE_THAN_CHANNEL_CAPACITY {
        let block = (mined_block(n).0, Height(1));
        match sender.try_send(block) {
            Ok(()) => assert!(accepted.insert(block.0)),
            Err(mpsc::error::TrySendError::Full(_)) => {
                backpressured = true;
                break;
            }
            Err(error) => panic!("gossip must keep the submission channel open: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    if !backpressured {
        gossip.abort();
    }
    assert!(
        backpressured,
        "unavailable peers must eventually backpressure independent committed submissions"
    );
    assert_eq!(
        accepted.len(),
        sender.max_capacity() + 16,
        "a full first-attempt queue must stop receiving committed notifications"
    );
    assert!(requests.try_recv().is_err());

    peer_set.set_readiness(Readiness::Ready);
    tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, async {
        while !accepted.is_empty() {
            let Some(zn::Request::AdvertiseBlockToAll(hash)) = requests.recv().await else {
                panic!("committed inventory must use all-peer delivery");
            };
            assert!(
                accepted.remove(&hash),
                "each accepted fork needs one attempt"
            );
        }
    })
    .await
    .expect("backpressure must not discard already accepted committed forks");
    gossip.abort();
}

/// Equal-height committed hashes carry no ancestry information and cannot supersede each other.
#[tokio::test(start_paused = true)]
async fn gossip_retains_independent_committed_forks_when_peers_become_ready() {
    let _init_guard = zebra_test::init();
    let (_tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();
    let channel = SubmitBlockChannel::new();
    let sender = channel.sender();
    let forks = [mined_block(1).0, mined_block(2).0];
    for hash in forks {
        sender.try_send((hash, Height(1))).unwrap();
    }
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        Some(channel.receiver()),
        Default::default(),
    ));
    tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, peer_set.waiting.notified())
        .await
        .expect("both submissions are consumed before readiness is available");
    assert!(requests.try_recv().is_err());
    peer_set.set_readiness(Readiness::Ready);

    let mut remaining: std::collections::HashSet<_> = forks.into_iter().collect();
    let result = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, async {
        for _ in forks {
            let Some(zn::Request::AdvertiseBlockToAll(hash)) = requests.recv().await else {
                panic!("committed inventory must use all-peer delivery");
            };
            assert!(remaining.remove(&hash), "forks must not be duplicated");
        }
    })
    .await;
    gossip.abort();
    result.expect("both independent committed forks must survive unavailable peers");
    assert!(remaining.is_empty());
}

async fn retry_unchanged_tip(times_out: bool) {
    let (mut tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    recent_syncs.push_extend_tips_length(0);
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let mut response = peer_set.hold_next_response();
    let hash = set_tip(&mut tip_sender, 1);
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set,
        None,
        Default::default(),
    ));
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlock(hash, None))
    );
    if times_out {
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, response.closed())
            .await
            .expect("the production network timeout cancels the pending response");
        drop(response);
    } else {
        response.send(Err("advert write failed".into())).unwrap();
    }
    tokio::task::yield_now().await;
    assert!(
        requests.try_recv().is_err(),
        "failed adverts must not busy-loop retries"
    );
    let retried = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, requests.recv()).await;
    gossip.abort();
    assert_eq!(
        retried.expect("the unchanged best tip must be retried"),
        Some(zn::Request::AdvertiseBlock(hash, None))
    );
}

/// An ordinary write failure must retry without another tip notification.
#[tokio::test(start_paused = true)]
async fn gossip_retries_unchanged_tip_after_failed_advert() {
    let _init_guard = zebra_test::init();
    retry_unchanged_tip(false).await;
}

/// A timed-out ordinary broadcast must retry without another tip notification.
#[tokio::test(start_paused = true)]
async fn gossip_retries_unchanged_tip_after_timed_out_advert() {
    let _init_guard = zebra_test::init();
    retry_unchanged_tip(true).await;
}

/// A newer tip supersedes a failed ordinary tip, including while readiness is unavailable.
#[tokio::test(start_paused = true)]
async fn gossip_new_tip_supersedes_failed_tip_retry() {
    let _init_guard = zebra_test::init();
    let (mut tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    recent_syncs.push_extend_tips_length(0);
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let response = peer_set.hold_next_response();
    let first = set_tip(&mut tip_sender, 1);
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        None,
        Default::default(),
    ));
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlock(first, None))
    );
    peer_set.set_readiness(Readiness::Pending);
    response.send(Err("advert write failed".into())).unwrap();
    tokio::time::sleep(Duration::from_millis(1)).await;
    let latest = set_tip(&mut tip_sender, 2);
    tokio::time::sleep(Duration::from_millis(1)).await;
    peer_set.set_readiness(Readiness::Ready);
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlock(latest, None))
    );
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    gossip.abort();
    assert!(
        requests.try_recv().is_err(),
        "a stale tip must not be retried"
    );
}

/// A failed side-chain advert has no best-tip notification to rescue it.
#[tokio::test(start_paused = true)]
async fn gossip_retries_side_chain_advert_after_another_submission() {
    let _init_guard = zebra_test::init();
    let (mut tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    recent_syncs.push_extend_tips_length(0);
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let channel = SubmitBlockChannel::new();
    let sender = channel.sender();
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        Some(channel.receiver()),
        Default::default(),
    ));
    let tip = set_tip(&mut tip_sender, 1);
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlock(tip, None))
    );
    let response = peer_set.hold_next_response();
    let forks = [mined_block(2).0, mined_block(3).0];
    sender.try_send((forks[0], Height(1))).unwrap();
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlockToAll(forks[0]))
    );
    peer_set.set_readiness(Readiness::Pending);
    response
        .send(Err("side-chain advert failed".into()))
        .unwrap();
    // Let the retry become overdue before the new first attempt can enter the scheduler.
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT + Duration::from_millis(1)).await;
    sender.try_send((forks[1], Height(1))).unwrap();
    peer_set.set_readiness(Readiness::Ready);
    let result = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, async {
        for expected in [forks[1], forks[0]] {
            let Some(zn::Request::AdvertiseBlockToAll(hash)) = requests.recv().await else {
                panic!("side-chain retries must remain committed all-peer adverts");
            };
            assert_eq!(hash, expected, "first attempts must precede due retries");
        }
    })
    .await;
    gossip.abort();
    result.expect("another submission must not erase the failed side-chain advert");
}

/// A full offline retry backlog must not delay a newly mined block after a peer connects.
#[tokio::test(start_paused = true)]
async fn gossip_fresh_mined_block_exhausts_oldest_pending_retry_without_delay() {
    let _init_guard = zebra_test::init();
    let (_tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let (sender, receiver) = mpsc::channel(16);
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        Some(receiver),
        Default::default(),
    ));
    let started = tokio::time::Instant::now();

    for n in 1..=16 {
        let response = peer_set.hold_next_response();
        sender.try_send(mined_block(n)).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(10), requests.recv())
                .await
                .unwrap(),
            Some(zn::Request::AdvertiseBlockToAll(mined_block(n).0)),
        );
        response.send(Err("no eligible peers".into())).unwrap();
        tokio::task::yield_now().await;
    }

    // The connection arrives halfway through the retry delay, as in fresh-link relay.
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT / 2).await;
    assert!(requests.try_recv().is_err());
    // A duplicate must not reclaim capacity or exhaust another hash's retry.
    sender.try_send(mined_block(1)).unwrap();
    tokio::task::yield_now().await;
    sender.try_send(mined_block(17)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(10), requests.recv())
            .await
            .expect("a new mined block must not wait for the offline backlog's retry deadline"),
        Some(zn::Request::AdvertiseBlockToAll(mined_block(17).0)),
    );
    assert!(started.elapsed() < TIPS_RESPONSE_TIMEOUT);

    // All other independent hashes retain their spaced retry, and the evicted hash never retries.
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT).await;
    for n in 2..=16 {
        assert_eq!(
            tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
                .await
                .unwrap(),
            Some(zn::Request::AdvertiseBlockToAll(mined_block(n).0)),
        );
    }
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    gossip.abort();
    assert!(
        requests.try_recv().is_err(),
        "the evicted oldest hash must not be advertised again"
    );
}

/// Exhausted offline relay work releases bounded capacity without erasing an unattempted fork.
#[tokio::test(start_paused = true)]
async fn gossip_exhausts_failed_committed_retry_and_attempts_next_fork() {
    let _init_guard = zebra_test::init();
    let (_tip_sender, _latest_tip, tip_change) = ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let first_response = peer_set.hold_next_response();
    let (sender, receiver) = mpsc::channel(1);
    sender.try_send(mined_block(1)).unwrap();
    let gossip = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        tip_change,
        peer_set.clone(),
        Some(receiver),
        Default::default(),
    ));
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlockToAll(mined_block(1).0))
    );
    let retry_response = peer_set.hold_next_response();
    first_response
        .send(Err("no eligible peers".into()))
        .unwrap();
    tokio::task::yield_now().await;
    assert!(
        requests.try_recv().is_err(),
        "the first fork retains its bounded retry slot"
    );
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT * 2, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlockToAll(mined_block(1).0))
    );
    sender.try_send(mined_block(2)).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        sender.capacity(),
        0,
        "an in-flight retry must keep backpressuring the next submission"
    );
    retry_response
        .send(Err("still no eligible peers".into()))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .unwrap(),
        Some(zn::Request::AdvertiseBlockToAll(mined_block(2).0))
    );
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    gossip.abort();
    assert!(
        requests.try_recv().is_err(),
        "exhausted committed retries must terminate"
    );
}

/// Real advertiser readiness retains committed mined blocks until a peer connects.
#[tokio::test(start_paused = true)]
async fn gossip_broadcasts_latest_mined_block_when_an_eligible_peer_connects() {
    let _init_guard = zebra_test::init();
    for supersede in [false, true] {
        let mut peers = zn::test_support::PeerSetHarness::new(&[], None).await;
        let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &Network::Mainnet);
        let (sync_status, _recent_syncs) = SyncStatus::new();
        let channel = SubmitBlockChannel::new();
        let mined_block_sender = channel.sender();
        let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
            sync_status,
            chain_tip_change,
            peers.block_advertiser.clone(),
            Some(channel.receiver()),
            Default::default(),
        ));

        let mut blocks = vec![mined_block(1)];
        mined_block_sender.try_send(blocks[0]).unwrap();
        tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
        assert!(!gossip_task.is_finished());
        if supersede {
            blocks.push(mined_block(2));
            mined_block_sender.try_send(blocks[1]).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        // Independent committed submissions are each advertised, oldest first.
        let peer = peers.connect_peer(zn::types::PeerServices::NODE_NETWORK);
        for (expected, _) in blocks {
            let (hash, write) = peers.next_block_advert(peer).await.unwrap();
            assert_eq!(hash, expected);
            write.send(Ok(())).unwrap();
        }
        tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
        assert!(
            peers.next_block_advert(peer).await.is_none(),
            "each committed block is advertised exactly once"
        );
        assert!(!gossip_task.is_finished());
        gossip_task.abort();
    }
}

/// Ready peers receive every queued committed block, including independent side-chain blocks.
#[tokio::test(start_paused = true)]
async fn gossip_preserves_queued_mined_blocks_when_peers_are_ready() {
    let _init_guard = zebra_test::init();
    let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &Network::Mainnet);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Ready);
    let channel = SubmitBlockChannel::new();
    let mined_block_sender = channel.sender();
    let blocks = [mined_block(1), mined_block(2)];
    for block in blocks {
        mined_block_sender
            .try_send(block)
            .expect("channel has capacity for both committed blocks");
    }

    let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        chain_tip_change,
        peer_set,
        Some(channel.receiver()),
        Default::default(),
    ));
    for (hash, _) in blocks {
        assert_eq!(
            tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
                .await
                .expect("ready peers receive each committed block"),
            Some(zn::Request::AdvertiseBlockToAll(hash)),
        );
    }
    gossip_task.abort();
}

/// A chain tip change that happens while there are no ready peers is broadcast when peers
/// become ready, without waiting for another tip change.
#[tokio::test(start_paused = true)]
async fn gossip_broadcasts_tip_change_when_peers_become_ready() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (mut chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();

    // Make the sync status close to the tip, so tip changes are gossiped.
    recent_syncs.push_extend_tips_length(0);

    let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        chain_tip_change,
        peer_set.clone(),
        None,
        Default::default(),
    ));

    let (hash, height) = mined_block(1);
    chain_tip_sender.set_finalized_tip(ChainTipBlock {
        hash,
        height,
        time: Utc::now(),
        transactions: Vec::new(),
        transaction_hashes: Arc::new([]),
        previous_block_hash: block::Hash([0xff; 32]),
    });

    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    assert!(
        requests.try_recv().is_err(),
        "no peers were ready, so nothing should have been broadcast"
    );

    peer_set.set_readiness(Readiness::Ready);

    let request = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
        .await
        .expect("tip change is broadcast once peers are ready")
        .expect("peer set is still alive");
    assert_eq!(request, zn::Request::AdvertiseBlock(hash, None));
    assert!(!gossip_task.is_finished());

    gossip_task.abort();
}

/// If a mined block is announced while a chain tip broadcast is waiting for ready peers, and the
/// mined block is not the best tip (for example, it is on a side chain), both the mined block and
/// the pending chain tip are broadcast when peers become ready.
#[tokio::test(start_paused = true)]
async fn gossip_keeps_pending_tip_change_when_a_side_chain_block_is_mined() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (mut chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();

    // Make the sync status close to the tip, so tip changes are gossiped.
    recent_syncs.push_extend_tips_length(0);

    let channel = SubmitBlockChannel::new();
    let mined_block_sender = channel.sender();

    let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        chain_tip_change,
        peer_set.clone(),
        Some(channel.receiver()),
        Default::default(),
    ));

    let (tip_hash, tip_height) = mined_block(1);
    chain_tip_sender.set_finalized_tip(ChainTipBlock {
        hash: tip_hash,
        height: tip_height,
        time: Utc::now(),
        transactions: Vec::new(),
        transaction_hashes: Arc::new([]),
        previous_block_hash: block::Hash([0xff; 32]),
    });

    // Wait until the chain tip broadcast is waiting for ready peers.
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT).await;

    // This block doesn't change the best tip.
    let (side_chain_hash, side_chain_height) = mined_block(2);
    mined_block_sender
        .try_send((side_chain_hash, side_chain_height))
        .expect("channel has capacity");

    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT).await;
    assert!(
        requests.try_recv().is_err(),
        "no peers were ready, so nothing should have been broadcast"
    );

    peer_set.set_readiness(Readiness::Ready);

    let mut broadcasts = Vec::new();
    for _ in 0..2 {
        let request = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
            .await
            .expect("both blocks are broadcast once peers are ready")
            .expect("peer set is still alive");
        broadcasts.push(request);
    }

    assert!(broadcasts.contains(&zn::Request::AdvertiseBlockToAll(side_chain_hash)));
    assert!(broadcasts.contains(&zn::Request::AdvertiseBlock(tip_hash, None)));
    assert!(!gossip_task.is_finished());

    gossip_task.abort();
}

/// Permanent peer set readiness errors still stop the gossip task.
#[tokio::test(start_paused = true)]
async fn gossip_returns_permanent_peer_set_errors() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, _requests) = GatedPeerSet::new();
    peer_set.set_readiness(Readiness::Failed);

    let channel = SubmitBlockChannel::new();
    channel
        .sender()
        .try_send(mined_block(1))
        .expect("channel has capacity");

    let result = tokio::time::timeout(
        TIPS_RESPONSE_TIMEOUT * 2,
        gossip_best_tip_block_hashes(
            sync_status,
            chain_tip_change,
            peer_set,
            Some(channel.receiver()),
            Default::default(),
        ),
    )
    .await
    .expect("gossip task exits on a permanent peer set error");

    assert!(
        matches!(result, Err(BlockGossipError::PeerSetReadiness(_))),
        "unexpected gossip result: {result:?}"
    );
}

/// Real validation populates the inbound cache; early adverts cannot replace committed inventory.
#[tokio::test(flavor = "multi_thread")]
async fn gossip_and_inbound_relay_only_validated_submissions() {
    use crate::components::inbound::{Inbound, InboundSetupData};
    use tower::{buffer::Buffer, util::BoxService, ServiceExt};
    use zebra_chain::{
        block::genesis::regtest_genesis_block, chain_sync_status::MockSyncStatus,
        serialization::ZcashSerialize,
    };
    use zebra_consensus::{Request as VerifyRequest, SemanticBlockVerifier};
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::{
        client::HexData,
        methods::{RpcImpl, RpcServer},
        SubmittedBlockCache,
    };
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::new_regtest(Default::default());
        let genesis = regtest_genesis_block();
        let (state, read_state, tip, _tip_changes) =
            zebra_state::init(zebra_state::Config::ephemeral(), &network, Height(0), 0).await;
        let state = Buffer::new(BoxService::new(state), 1);
        state
            .clone()
            .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
                genesis.clone().into(),
            ))
            .await
            .unwrap();
        let transactions = Buffer::new(
            zebra_consensus::transaction::BlockTxVerifier::new(&network, state.clone()),
            1,
        );
        let semantic = SemanticBlockVerifier::new(&network, state.clone(), transactions);
        let prepared_blocks = semantic.prepared_blocks();
        let real_verifier = Buffer::new(
            BoxService::new(
                semantic.map_err(|source| zebra_consensus::RouterError::Block {
                    source: Box::new(source),
                }),
            ),
            1,
        );
        let inbound_verifier = real_verifier.clone();
        let (completed, mut completions) = mpsc::channel(1);
        let verifier = tower::service_fn(move |request: VerifyRequest| {
            let verifier = real_verifier.clone();
            let completed = completed.clone();
            async move {
                let mined = matches!(
                    &request,
                    VerifyRequest::CommitMined(_, _) | VerifyRequest::CommitDecoded(_, Some(_))
                );
                let hash = request.block().hash();
                let result = verifier.oneshot(request).await;
                if mined {
                    // Hold only the real verifier's reply, never fabricate a validation verdict.
                    let (release, released) = oneshot::channel();
                    completed.send((hash, release)).await.unwrap();
                    released.await.unwrap();
                }
                result
            }
        });
        let mempool = tower::service_fn({
            let read_state = read_state.clone();
            move |request| {
                let read_state = read_state.clone();
                async move {
                    assert!(matches!(request, mempool::Request::FullTransactions));
                    let zebra_state::ReadResponse::Tip(Some((_, hash))) = read_state
                        .oneshot(zebra_state::ReadRequest::Tip)
                        .await
                        .unwrap()
                    else {
                        panic!("genesis was committed before constructing the mempool fixture");
                    };
                    Ok::<_, BoxError>(mempool::Response::FullTransactions {
                        transactions: vec![],
                        transaction_dependencies: Default::default(),
                        last_seen_tip_hash: hash,
                    })
                }
            }
        });
        let cache = SubmittedBlockCache::default();
        let mut validated = cache.subscribe();
        validated.borrow_and_update();
        let committed = SubmitBlockChannel::new();
        let committed_sender = committed.sender();
        let mut committed = committed.receiver();
        let mut synced = MockSyncStatus::default();
        synced.set_is_close_to_tip(true);
        let (_logs, logs) = tokio::sync::watch::channel(None);
        let (rpc, queue) = RpcImpl::new(
            network.clone(),
            zebra_rpc::config::mining::Config {
                miner_address: Some("tmVqEASZxBNKFTbmASZikGa5fPLkd68iJyx".parse().unwrap()),
                ..Default::default()
            },
            false,
            "0.0.1",
            "validated relay test",
            mempool.clone(),
            state,
            read_state,
            verifier.clone(),
            synced,
            tip,
            MockAddressBookPeers::default(),
            logs,
            Some(committed_sender),
        );
        queue.abort();
        let rpc = rpc
            .with_submitted_blocks(cache.clone())
            .with_prepared_blocks(prepared_blocks);
        let mempool_changes =
            mempool::MempoolTxSubscriber::new(tokio::sync::broadcast::channel(1).0);
        let updater = rpc.spawn_block_template_updater(&mempool_changes).unwrap();

        let (mut chain_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &network);
        let (sync_status, mut recent_syncs) = SyncStatus::new();
        recent_syncs.push_extend_tips_length(0);
        let (peer_set, mut requests) = GatedPeerSet::new();
        let (setup_tx, setup_rx) = oneshot::channel();
        let (misbehavior_sender, _misbehavior_rx) = mpsc::channel(1);
        let mut fallback_state: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();
        assert!(setup_tx
            .send(InboundSetupData {
                address_book: Arc::new(Mutex::new(zn::AddressBook::new(
                    "127.0.0.1:0".parse().unwrap(),
                    zn::types::PeerServices::NODE_NETWORK,
                    &network,
                    zn::constants::DEFAULT_MAX_CONNS_PER_IP,
                    tracing::Span::none(),
                ))),
                block_download_peer_set: Buffer::new(BoxService::new(peer_set.clone()), 1),
                block_verifier: inbound_verifier,
                mempool: Buffer::new(BoxService::new(mempool), 1),
                state: Buffer::new(BoxService::new(fallback_state.clone()), 1),
                latest_chain_tip,
                misbehavior_sender,
            })
            .is_ok());
        let mut inbound = Inbound::new(1, setup_rx).with_submitted_blocks(cache.clone());
        chain_sender.set_finalized_tip(ChainTipBlock {
            hash: genesis.hash(),
            height: Height(0),
            time: Utc::now(),
            transactions: Vec::new(),
            transaction_hashes: Arc::new([]),
            previous_block_hash: genesis.header.previous_block_hash,
        });
        // Gate actual state-emitted events to test consumer ordering, not producer eligibility.
        let (gossip_sender, gossip_receiver) = mpsc::channel(2);
        let gossip = tokio::spawn(gossip_best_tip_block_hashes(
            sync_status,
            chain_tip_change,
            peer_set.clone(),
            Some(gossip_receiver),
            cache.clone(),
        ));
        peer_set.waiting.notified().await;
        let mut first_commit = None;
        let mut last_candidate = None;
        for nonce in [1_u8, 2] {
            peer_set.set_readiness(Readiness::Pending);
            let _ = futures::FutureExt::now_or_never(peer_set.waiting.notified());
            let template = rpc
                .get_block_template(None)
                .await
                .unwrap()
                .try_into_template()
                .unwrap();
            let proposal =
                zebra_rpc::proposal_block_from_template(&template, None, &network).unwrap();
            verifier
                .clone()
                .oneshot(VerifyRequest::CheckProposal(Arc::new(proposal.clone())))
                .await
                .unwrap();
            let mut candidate = proposal;
            Arc::make_mut(&mut candidate.header).nonce = [nonce; 32].into();
            let candidate = Arc::new(candidate);
            let hash = candidate.hash();
            assert!(cache.get(&hash).is_none());
            let call = tokio::spawn({
                let rpc = rpc.clone();
                let bytes = candidate.zcash_serialize_to_vec().unwrap();
                async move { rpc.submit_block(HexData(bytes), None).await }
            });
            let (completed_hash, release) = completions.recv().await.unwrap();
            assert_eq!(completed_hash, hash);
            validated.changed().await.unwrap();
            assert_eq!(
                (*validated.borrow_and_update()).map(|(hash, _)| hash),
                Some(hash)
            );
            assert_eq!(cache.get(&hash), Some(candidate.clone()));
            assert!(!call.is_finished(), "the real verifier reply is still held");
            let event = committed.recv().await.unwrap();
            assert_eq!(event, (hash, Height(u32::from(nonce))));
            assert_eq!(
                inbound
                    .ready()
                    .await
                    .unwrap()
                    .call(zn::Request::BlocksByHash([hash].into()))
                    .await
                    .unwrap(),
                zn::Response::Blocks(vec![zn::InventoryResponse::Available((
                    candidate.clone(),
                    None
                ))]),
            );
            if nonce == 1 {
                peer_set.set_readiness(Readiness::Ready);
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlock(genesis.hash(), None))
                );
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlock(hash, None))
                );
                first_commit = Some(event);
            } else {
                // The second validated body is waiting for sampled delivery. A committed event
                // must take priority and withdraw that pending early advert, without losing either
                // independently committed hash.
                peer_set.waiting.notified().await;
                let first = first_commit.take().unwrap();
                gossip_sender.send(first).await.unwrap();
                peer_set.set_readiness(Readiness::Ready);
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlockToAll(first.0))
                );
                gossip_sender.send(event).await.unwrap();
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlockToAll(hash))
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), requests.recv())
                        .await
                        .is_err()
                );
            }
            release.send(()).unwrap();
            assert_eq!(
                call.await.unwrap().unwrap(),
                zebra_rpc::client::SubmitBlockResponse::Accepted
            );
            last_candidate = Some(candidate);
        }

        let mut invalid = (*last_candidate.unwrap()).clone();
        Arc::make_mut(&mut invalid.header).merkle_root = [0xff; 32].into();
        let hash = invalid.hash();
        let rejected = tokio::spawn({
            let rpc = rpc.clone();
            let bytes = invalid.zcash_serialize_to_vec().unwrap();
            async move { rpc.submit_block(HexData(bytes), None).await }
        });
        let (completed_hash, release) = completions.recv().await.unwrap();
        assert_eq!(completed_hash, hash);
        assert!(
            cache.get(&hash).is_none(),
            "real failed validation must not publish a body"
        );
        let missing = inbound
            .ready()
            .await
            .unwrap()
            .call(zn::Request::BlocksByHash([hash].into()));
        tokio::pin!(missing);
        assert!(futures::poll!(&mut missing).is_pending());
        fallback_state
            .expect_request(zebra_state::Request::Block(hash.into()))
            .await
            .respond(zebra_state::Response::Block(None));
        assert_eq!(
            missing.await.unwrap(),
            zn::Response::Blocks(vec![zn::InventoryResponse::Missing(hash)])
        );
        assert!(
            committed.try_recv().is_err(),
            "rejected bodies have no committed event"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), requests.recv())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(
            rejected.await.unwrap().unwrap(),
            zebra_rpc::client::SubmitBlockErrorResponse::Rejected.into(),
        );
        gossip.abort();
        updater.abort();
    })
    .await
    .expect("real validation and gossip consumer ordering must finish");
}
