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

use chrono::Utc;
use futures::future;
use tokio::sync::mpsc;
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
        gossip_best_tip_block_hashes, BlockGossipError, SyncStatus, PEER_GOSSIP_DELAY,
        TIPS_RESPONSE_TIMEOUT,
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
    waiting: Arc<tokio::sync::Notify>,
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
            waiting: Default::default(),
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
                self.waiting.notify_one();
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
        Default::default(),
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

/// Mined blocks that are announced while there are no ready peers are coalesced, and the latest
/// one is broadcast when peers become ready, without waiting for another block.
#[tokio::test(start_paused = true)]
async fn gossip_broadcasts_latest_mined_block_when_peers_become_ready() {
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
        Default::default(),
    ));

    mined_block_sender
        .try_send(mined_block(1))
        .expect("channel has capacity");

    // The latest block is announced just before the first block has waited for the entire
    // response timeout, so it is still fresh when peers become ready.
    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT - Duration::from_millis(1)).await;
    let (latest_hash, latest_height) = mined_block(2);
    mined_block_sender
        .try_send((latest_hash, latest_height))
        .expect("channel has capacity");

    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    assert!(
        requests.try_recv().is_err(),
        "no peers were ready, so nothing should have been broadcast"
    );

    peer_set.set_readiness(Readiness::Ready);

    let request = tokio::time::timeout(TIPS_RESPONSE_TIMEOUT, requests.recv())
        .await
        .expect("latest mined block is broadcast once peers are ready")
        .expect("peer set is still alive");
    assert_eq!(request, zn::Request::AdvertiseBlockToAll(latest_hash));

    tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
    assert!(
        requests.try_recv().is_err(),
        "superseded mined blocks should not be broadcast"
    );
    assert!(!gossip_task.is_finished());

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

    tokio::time::sleep(PEER_GOSSIP_DELAY + TIPS_RESPONSE_TIMEOUT * 2).await;
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
    tokio::time::sleep(PEER_GOSSIP_DELAY + TIPS_RESPONSE_TIMEOUT).await;

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

/// Pending bodies are served before verification; rejecting one cannot displace a committed advert.
#[tokio::test]
async fn gossip_and_inbound_relay_submissions_before_verification() {
    use crate::components::inbound::{Inbound, InboundSetupData};
    use tower::{buffer::Buffer, util::BoxService, ServiceExt};
    use zebra_chain::{
        chain_sync_status::MockSyncStatus,
        chain_tip::mock::MockChainTip,
        parameters::{testnet::Parameters, NetworkUpgrade},
        serialization::{DateTime32, ZcashSerialize},
        work::difficulty::ParameterDifficulty,
    };
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_node_services::mempool;
    use zebra_rpc::{
        client::HexData,
        methods::{RpcImpl, RpcServer},
        SubmittedBlockCache,
    };
    use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Parameters::build()
            .with_disable_pow(true)
            .to_network()
            .unwrap();
        let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
        let tip_hash = block::Hash([1; 32]);
        let chain_info = GetBlockTemplateChainInfo {
            tip_hash,
            tip_height: height,
            chain_history_root: Some([0; 32].into()),
            expected_difficulty: network.target_difficulty_limit().to_compact(),
            cur_time: DateTime32::now(),
            min_time: DateTime32::now(),
            max_time: DateTime32::now(),
            chain_value_pools: Default::default(),
        };
        let read_state = tower::service_fn(move |request| {
            let info = chain_info.clone();
            async move {
                Ok::<_, BoxError>(match request {
                    ReadRequest::Tip => ReadResponse::Tip(Some((info.tip_height, info.tip_hash))),
                    ReadRequest::ChainInfo => ReadResponse::ChainInfo(info),
                    other => panic!("unexpected state request: {other:?}"),
                })
            }
        });
        let (tip, tip_sender) = MockChainTip::new();
        tip_sender.send_best_tip_hash(tip_hash);
        tip_sender.send_best_tip_height(height);
        tip_sender.send_best_tip_block_time(Utc::now());
        let (_chain_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &network);
        let (sync_status, _recent_syncs) = SyncStatus::new();
        let (peer_set, mut requests) = GatedPeerSet::new();
        peer_set.set_readiness(Readiness::Ready);
        let cache = SubmittedBlockCache::default();
        let channel = SubmitBlockChannel::new();
        let mut verifier: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
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
            "relay test",
            mempool.clone(),
            state.clone(),
            read_state,
            verifier.clone(),
            synced,
            tip,
            MockAddressBookPeers::default(),
            logs,
            Some(channel.sender()),
        );
        queue.abort();
        let rpc = rpc.with_submitted_blocks(cache.clone(), Height(0));
        let updater = rpc.spawn_block_template_updater().unwrap();
        // The full build reads the mempool only after publishing the coinbase-only template.
        let full_build = mempool
            .expect_request(mempool::Request::FullTransactions)
            .await;
        let template = rpc
            .get_block_template(None)
            .await
            .unwrap()
            .try_into_template()
            .unwrap();
        let block = zebra_rpc::proposal_block_from_template(&template, None, &network).unwrap();
        full_build.respond(mempool::Response::FullTransactions {
            transactions: vec![],
            transaction_dependencies: Default::default(),
            last_seen_tip_hash: tip_hash,
        });
        updater.abort();

        let (setup_tx, setup_rx) = tokio::sync::oneshot::channel();
        let (misbehavior_sender, _misbehavior_rx) = mpsc::channel(1);
        assert!(setup_tx
            .send(InboundSetupData {
                address_book: Arc::new(std::sync::Mutex::new(zn::AddressBook::new(
                    "127.0.0.1:0".parse().unwrap(),
                    &network,
                    zn::constants::DEFAULT_MAX_CONNS_PER_IP,
                    tracing::Span::none(),
                ))),
                block_download_peer_set: Buffer::new(BoxService::new(peer_set.clone()), 1),
                block_verifier: Buffer::new(
                    BoxService::new(MockService::build().for_unit_tests()),
                    1
                ),
                mempool: Buffer::new(BoxService::new(mempool), 1),
                state: Buffer::new(BoxService::new(state.clone()), 1),
                latest_chain_tip,
                misbehavior_sender,
            })
            .is_ok());
        let mut inbound = Inbound::new(1, setup_rx).with_submitted_blocks(cache.clone());
        let gossip = tokio::spawn(gossip_best_tip_block_hashes(
            sync_status,
            chain_tip_change,
            peer_set.clone(),
            Some(channel.receiver()),
            cache.clone(),
        ));

        let mut committed_hash = None;
        for n in 0..3 {
            let mut candidate = block.clone();
            Arc::make_mut(&mut candidate.header).nonce = [n; 32].into();
            let candidate = Arc::new(candidate);
            let hash = candidate.hash();
            let call = tokio::spawn({
                let rpc = rpc.clone();
                let bytes = candidate.zcash_serialize_to_vec().unwrap();
                async move { rpc.submit_block(HexData(bytes), None).await }
            });
            let verification = verifier
                .expect_request(zebra_consensus::Request::Commit(candidate.clone()))
                .await;
            assert!(!call.is_finished());
            assert_eq!(cache.get(&hash), Some(candidate.clone()));
            if n < 2 {
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlockToAll(hash))
                );
            }
            let (call, verification) = if n == 0 {
                let duplicate = tokio::spawn({
                    let rpc = rpc.clone();
                    let bytes = candidate.zcash_serialize_to_vec().unwrap();
                    async move { rpc.submit_block(HexData(bytes), None).await }
                });
                let duplicate_verification = verifier
                    .expect_request(zebra_consensus::Request::Commit(candidate.clone()))
                    .await;
                verification.respond_error(
                    zebra_consensus::RouterError::Block {
                        source: Box::new(zebra_consensus::VerifyBlockError::Block {
                            source: zebra_consensus::BlockError::AlreadyInChain(
                                hash,
                                zebra_state::KnownBlock::BestChain,
                            ),
                        }),
                    }
                    .into(),
                );
                assert_eq!(
                    call.await.unwrap().unwrap(),
                    zebra_rpc::client::SubmitBlockErrorResponse::Duplicate.into()
                );
                assert!(!duplicate.is_finished());
                (duplicate, duplicate_verification)
            } else {
                (call, verification)
            };
            if n == 0 {
                assert_eq!(
                    inbound
                        .ready()
                        .await
                        .unwrap()
                        .call(zn::Request::BlocksByHash([hash].into()))
                        .await
                        .unwrap(),
                    zn::Response::Blocks(vec![zn::InventoryResponse::Available((candidate, None))])
                );
            }
            if n == 1 {
                peer_set.set_readiness(Readiness::Pending);
                verification.respond(hash);
                committed_hash = Some(hash);
                assert_eq!(
                    call.await.unwrap().unwrap(),
                    zebra_rpc::client::SubmitBlockResponse::Accepted
                );
                peer_set.waiting.notified().await;
            } else {
                verification.respond_error(
                    zebra_consensus::RouterError::Block {
                        source: Box::new(zebra_consensus::VerifyBlockError::Transaction(
                            zebra_consensus::error::TransactionError::Other(
                                "transaction has Orchard actions (temporarily disabled)".into(),
                            ),
                        )),
                    }
                    .into(),
                );
                assert_eq!(
                    call.await.unwrap().unwrap(),
                    zebra_rpc::client::SubmitBlockErrorResponse::Rejected.into()
                );
            }
            assert_eq!(cache.get(&hash).is_some(), n == 1);
            if n == 0 {
                let request = inbound
                    .ready()
                    .await
                    .unwrap()
                    .call(zn::Request::BlocksByHash([hash].into()));
                tokio::pin!(request);
                assert!(futures::poll!(&mut request).is_pending());
                state
                    .expect_request(zebra_state::Request::Block(hash.into()))
                    .await
                    .respond(zebra_state::Response::Block(None));
                assert_eq!(
                    request.await.unwrap(),
                    zn::Response::Blocks(vec![zn::InventoryResponse::Missing(hash)])
                );
            }
        }
        peer_set.set_readiness(Readiness::Ready);
        assert_eq!(
            requests.recv().await,
            Some(zn::Request::AdvertiseBlockToAll(committed_hash.unwrap()))
        );
        gossip.abort();
    })
    .await
    .expect("pending relay and committed advertisement regression must finish");
}
