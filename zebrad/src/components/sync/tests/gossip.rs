//! Tests for the block hash gossip task.
//!
//! These tests use a peer service with controllable readiness, because the bug they cover
//! (<https://github.com/ZcashFoundation/zebra/issues/11475>) only happens when `poll_ready()`
//! stays pending, which `tower::timeout::Timeout` doesn't bound.

use std::{
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Duration,
};

use futures::future;
use tokio::sync::mpsc;
use tower::Service;

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
};
use zebra_network as zn;
use zebra_rpc::SubmitBlockChannel;
use zebra_state::ChainTipSender;

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
}

/// A fake peer set whose `poll_ready()` is controlled by the test,
/// and which forwards all requests it receives to the test.
#[derive(Clone, Debug)]
struct GatedPeerSet {
    state: Arc<Mutex<GateState>>,
    requests: mpsc::UnboundedSender<zn::Request>,
}

impl GatedPeerSet {
    fn new() -> (Self, mpsc::UnboundedReceiver<zn::Request>) {
        let (requests, request_receiver) = mpsc::unbounded_channel();
        let peer_set = GatedPeerSet {
            state: Arc::new(Mutex::new(GateState {
                readiness: Readiness::Pending,
                wakers: Vec::new(),
            })),
            requests,
        };

        (peer_set, request_receiver)
    }

    fn set_readiness(&self, readiness: Readiness) {
        let mut state = self.state.lock().expect("lock is not poisoned");
        state.readiness = readiness;
        state.wakers.drain(..).for_each(Waker::wake);
    }
}

impl Service<zn::Request> for GatedPeerSet {
    type Response = zn::Response;
    type Error = BoxError;
    type Future = future::Ready<Result<zn::Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let mut state = self.state.lock().expect("lock is not poisoned");
        match state.readiness {
            Readiness::Pending => {
                state.wakers.push(cx.waker().clone());
                Poll::Pending
            }
            Readiness::Ready => Poll::Ready(Ok(())),
            Readiness::Failed => Poll::Ready(Err("permanent peer set failure".into())),
        }
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        let _ = self.requests.send(request);
        future::ok(zn::Response::Nil)
    }
}

fn mined_block(n: u32) -> (block::Hash, Height) {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&n.to_le_bytes());
    (block::Hash(hash), Height(n))
}

/// If there are never any ready peers, the gossip task must keep consuming mined block
/// announcements, so `submitblock` never sees a full channel.
#[tokio::test(start_paused = true)]
async fn gossip_keeps_draining_mined_blocks_without_ready_peers() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();

    let channel = SubmitBlockChannel::new();
    let mined_block_sender = channel.sender();

    let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        chain_tip_change,
        peer_set,
        Some(channel.receiver()),
    ));

    for n in 0..MORE_THAN_CHANNEL_CAPACITY {
        mined_block_sender
            .try_send(mined_block(n))
            .unwrap_or_else(|error| {
                panic!("mined block {n} could not be queued for gossip: {error:?}")
            });

        // Blocks are committed much faster than the readiness timeout.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        !gossip_task.is_finished(),
        "gossip task must keep running without ready peers"
    );
    assert!(
        requests.try_recv().is_err(),
        "no peers were ready, so nothing should have been broadcast"
    );

    gossip_task.abort();
}

/// Once peers become ready again, mined blocks are broadcast as usual.
#[tokio::test(start_paused = true)]
async fn gossip_resumes_broadcasting_when_peers_become_ready() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (_chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (peer_set, mut requests) = GatedPeerSet::new();

    let channel = SubmitBlockChannel::new();
    let mined_block_sender = channel.sender();

    let gossip_task = tokio::spawn(gossip_best_tip_block_hashes(
        sync_status,
        chain_tip_change,
        peer_set.clone(),
        Some(channel.receiver()),
    ));

    // This announcement is skipped, because there are no ready peers.
    mined_block_sender
        .try_send(mined_block(1))
        .expect("channel has capacity");
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    assert!(requests.try_recv().is_err());

    peer_set.set_readiness(Readiness::Ready);

    let (hash, height) = mined_block(2);
    mined_block_sender
        .try_send((hash, height))
        .expect("channel has capacity");

    let request = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
        .await
        .expect("mined block is broadcast once peers are ready")
        .expect("peer set is still alive");
    assert_eq!(request, zn::Request::AdvertiseBlockToAll(hash));
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
        ),
    )
    .await
    .expect("gossip task exits on a permanent peer set error");

    assert!(
        matches!(result, Err(BlockGossipError::PeerSetReadiness(_))),
        "unexpected gossip result: {result:?}"
    );
}
