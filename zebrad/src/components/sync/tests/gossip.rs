//! Tests for the block hash gossip task.
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

/// Real advertiser readiness retains the latest mined block until a peer connects.
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

        let mut latest_block = mined_block(1);
        mined_block_sender.try_send(latest_block).unwrap();
        tokio::time::sleep(TIPS_RESPONSE_TIMEOUT * 2).await;
        assert!(!gossip_task.is_finished());
        if supersede {
            latest_block = mined_block(2);
            mined_block_sender.try_send(latest_block).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        let peer = peers.connect_peer(zn::types::PeerServices::NODE_NETWORK);
        let (hash, write) = peers.next_block_advert(peer).await.unwrap();
        assert_eq!(hash, latest_block.0);
        write.send(Ok(())).unwrap();
        tokio::time::sleep(PEER_GOSSIP_DELAY * 2).await;
        assert!(
            peers.next_block_advert(peer).await.is_none(),
            "only the latest mined block is advertised, exactly once"
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
        let real_verifier = Buffer::new(
            BoxService::new(
                SemanticBlockVerifier::new(&network, state.clone(), transactions).map_err(
                    |source| zebra_consensus::RouterError::Block {
                        source: Box::new(source),
                    },
                ),
            ),
            1,
        );
        let inbound_verifier = real_verifier.clone();
        let (completed, mut completions) = mpsc::channel(1);
        let verifier = tower::service_fn(move |request: VerifyRequest| {
            let verifier = real_verifier.clone();
            let completed = completed.clone();
            async move {
                let mined = matches!(&request, VerifyRequest::CommitMined(_, _));
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
        let rpc = rpc.with_submitted_blocks(cache.clone());
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
                    Some(zn::Request::AdvertiseBlock(hash, None))
                );
                assert_eq!(
                    requests.recv().await,
                    Some(zn::Request::AdvertiseBlock(genesis.hash(), None))
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
