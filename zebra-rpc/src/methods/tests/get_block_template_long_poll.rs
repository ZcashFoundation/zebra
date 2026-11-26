//! Tests for `getblocktemplate` long-poll admission bounds and lifetime limits.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use jsonrpsee_types::{ErrorCode, ErrorObjectOwned};
use tokio::sync::watch;
use tower::Service;
use zcash_address::{ToAddress, ZcashAddress};
use zcash_protocol::consensus::NetworkType;
use zebra_chain::{
    block::Hash,
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::{mempool, BoxError};
use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::{
    config::mining,
    methods::{
        tests::utils::fake_history_tree,
        types::{
            get_block_template::{
                constants::{MAX_LONG_POLL_DURATION, MEMPOOL_LONG_POLL_INTERVAL},
                BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateResponse,
            },
            long_poll::{LongPollId, LongPollInput},
        },
        RpcImpl, RpcServer,
    },
};

#[derive(Clone)]
struct TestReadState {
    chain_info_rx: watch::Receiver<GetBlockTemplateChainInfo>,
    chain_info_calls: Arc<AtomicU32>,
    stall_chain_info: Arc<AtomicBool>,
    stall_tip_rx: watch::Receiver<bool>,
    yield_tip: Arc<AtomicBool>,
}

impl Service<ReadRequest> for TestReadState {
    type Response = ReadResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ReadRequest) -> Self::Future {
        let chain_info_rx = self.chain_info_rx.clone();
        let chain_info_calls = self.chain_info_calls.clone();
        let stall = self.stall_chain_info.load(Ordering::SeqCst);
        let mut stall_tip_rx = self.stall_tip_rx.clone();
        let yield_tip = self.yield_tip.load(Ordering::SeqCst);
        Box::pin(async move {
            match request {
                ReadRequest::Tip => {
                    let _ = stall_tip_rx.wait_for(|stalled| !*stalled).await;
                    if yield_tip {
                        tokio::task::yield_now().await;
                    }
                    let chain_info = chain_info_rx.borrow().clone();
                    Ok(ReadResponse::Tip(Some((
                        chain_info.tip_height,
                        chain_info.tip_hash,
                    ))))
                }
                ReadRequest::ChainInfo => {
                    chain_info_calls.fetch_add(1, Ordering::SeqCst);
                    if stall {
                        std::future::pending().await
                    } else {
                        Ok(ReadResponse::ChainInfo(chain_info_rx.borrow().clone()))
                    }
                }
                other => panic!("unexpected state request: {other:?}"),
            }
        })
    }
}

#[derive(Clone)]
struct TestMempool {
    tip_rx: watch::Receiver<Hash>,
    stall: Arc<AtomicBool>,
}

impl Service<mempool::Request> for TestMempool {
    type Response = mempool::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: mempool::Request) -> Self::Future {
        let tip_hash = *self.tip_rx.borrow();
        let stall = self.stall.load(Ordering::SeqCst);
        Box::pin(async move {
            assert!(matches!(request, mempool::Request::FullTransactions));
            if stall {
                return std::future::pending().await;
            }
            Ok(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: tip_hash,
            })
        })
    }
}

type TestRpc = RpcImpl<
    TestMempool,
    MockService<
        zebra_state::Request,
        zebra_state::Response,
        zebra_test::mock_service::PanicAssertion,
    >,
    TestReadState,
    MockChainTip,
    MockAddressBookPeers,
    MockService<
        zebra_consensus::Request,
        zebra_chain::block::Hash,
        zebra_test::mock_service::PanicAssertion,
    >,
    MockSyncStatus,
>;

struct LongPollHarness {
    rpc: TestRpc,
    queue: tokio::task::JoinHandle<()>,
    net: Network,
    chain_info: GetBlockTemplateChainInfo,
    chain_info_tx: watch::Sender<GetBlockTemplateChainInfo>,
    mempool_tip_tx: watch::Sender<Hash>,
    mock_tip_sender: MockChainTipSender,
    chain_info_calls: Arc<AtomicU32>,
    stall_state_chain_info: Arc<AtomicBool>,
    stall_mempool: Arc<AtomicBool>,
    stall_tip_tx: watch::Sender<bool>,
    yield_tip: Arc<AtomicBool>,
}

fn test_chain_info(net: &Network, max_time_offset_secs: i64) -> GetBlockTemplateChainInfo {
    let tip_height = NetworkUpgrade::Nu5.activation_height(net).unwrap();
    let tip_hash = Hash([1; 32]);
    let now = DateTime32::from(1_700_000_000);
    let max_time =
        DateTime32::from(u32::try_from(i64::from(now.timestamp()) + max_time_offset_secs).unwrap());
    GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        tip_height,
        tip_hash,
        cur_time: now,
        min_time: now,
        max_time,
        chain_history_root: fake_history_tree(net).hash(),
        chain_value_pools: Default::default(),
    }
}

impl LongPollHarness {
    fn new(max_long_polls: usize, max_time_offset_secs: i64) -> Self {
        let net = Network::Mainnet;
        let chain_info = test_chain_info(&net, max_time_offset_secs);
        let (chain_info_tx, chain_info_rx) = watch::channel(chain_info.clone());
        let (mempool_tip_tx, mempool_tip_rx) = watch::channel(chain_info.tip_hash);
        let chain_info_calls = Arc::new(AtomicU32::new(0));
        let stall_state_chain_info = Arc::new(AtomicBool::new(false));
        let stall_mempool = Arc::new(AtomicBool::new(false));
        let (stall_tip_tx, stall_tip_rx) = watch::channel(false);
        let yield_tip = Arc::new(AtomicBool::new(false));

        let read_state = TestReadState {
            chain_info_rx,
            chain_info_calls: chain_info_calls.clone(),
            stall_chain_info: stall_state_chain_info.clone(),
            stall_tip_rx,
            yield_tip: yield_tip.clone(),
        };
        let mempool = TestMempool {
            tip_rx: mempool_tip_rx,
            stall: stall_mempool.clone(),
        };

        let (mock_tip, mock_tip_sender) = MockChainTip::new();
        mock_tip_sender.send_best_tip_height(chain_info.tip_height);
        mock_tip_sender.send_best_tip_hash(chain_info.tip_hash);
        mock_tip_sender.send_best_tip_block_time(chrono::Utc::now());
        let mut sync = MockSyncStatus::default();
        sync.set_is_close_to_tip(true);
        let (_tx, rx) = watch::channel(None);

        let (mut rpc, queue) = RpcImpl::new(
            net.clone(),
            mining::Config {
                miner_address: Some(ZcashAddress::from_transparent_p2pkh(
                    NetworkType::Main,
                    [0x7e; 20],
                )),
                ..Default::default()
            },
            false,
            "0.0.1",
            "RPC test",
            mempool,
            MockService::build().for_unit_tests(),
            read_state,
            MockService::build().for_unit_tests(),
            sync,
            mock_tip,
            MockAddressBookPeers::default(),
            rx,
            None,
        );
        rpc = rpc.with_max_long_polls(max_long_polls);
        rpc.template_clock = Some(Arc::new(AtomicU32::new(chain_info.cur_time.timestamp())));

        Self {
            rpc,
            queue,
            net,
            chain_info,
            chain_info_tx,
            mempool_tip_tx,
            mock_tip_sender,
            chain_info_calls,
            stall_state_chain_info,
            stall_mempool,
            stall_tip_tx,
            yield_tip,
        }
    }

    fn make_template(&self, info: &GetBlockTemplateChainInfo) -> BlockTemplateResponse {
        BlockTemplateResponse::new_internal(
            &self.net,
            &self.rpc.gbt.coinbase_cache(),
            self.rpc.gbt.miner_params().unwrap(),
            info,
            LongPollInput::new(
                info.tip_height,
                info.tip_hash,
                info.max_time,
                std::iter::empty(),
            )
            .generate_id(),
            vec![],
            None,
        )
    }

    fn next_chain_info(&self) -> GetBlockTemplateChainInfo {
        let mut next = self.chain_info.clone();
        next.tip_height = next.tip_height.next().unwrap();
        next.tip_hash = Hash([2; 32]);
        next.max_time = next.max_time.saturating_add(Duration32::from_seconds(30));
        next
    }

    fn advance_tip(&self, next: &GetBlockTemplateChainInfo) {
        self.mock_tip_sender.send_best_tip_height(next.tip_height);
        self.mock_tip_sender.send_best_tip_hash(next.tip_hash);
        self.chain_info_tx.send_replace(next.clone());
        self.mempool_tip_tx.send_replace(next.tip_hash);
    }

    fn publish_template(&self, template: &BlockTemplateResponse) {
        self.rpc
            .gbt
            .template_cache()
            .unwrap()
            .publish(template.clone());
    }
}

impl Drop for LongPollHarness {
    fn drop(&mut self) {
        self.queue.abort();
    }
}

type ParkedLongPollFuture<'a> =
    Pin<Box<dyn Future<Output = Result<GetBlockTemplateResponse, ErrorObjectOwned>> + 'a>>;

fn long_poll_params(id: LongPollId) -> Option<GetBlockTemplateParameters> {
    Some(GetBlockTemplateParameters {
        long_poll_id: Some(id),
        ..Default::default()
    })
}

async fn park_two_long_polls(
    harness: &LongPollHarness,
    id: LongPollId,
) -> (ParkedLongPollFuture<'_>, ParkedLongPollFuture<'_>) {
    let mut lp1: ParkedLongPollFuture<'_> =
        Box::pin(harness.rpc.get_block_template(long_poll_params(id)));
    let mut lp2: ParkedLongPollFuture<'_> =
        Box::pin(harness.rpc.get_block_template(long_poll_params(id)));
    assert!(futures::poll!(&mut lp1).is_pending());
    assert!(futures::poll!(&mut lp2).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 2);
    (lp1, lp2)
}

async fn assert_excess_long_poll_rejected_immediately(
    harness: &LongPollHarness,
    id: LongPollId,
    expected_active: usize,
) {
    let err = tokio::time::timeout(
        Duration::from_millis(100),
        harness.rpc.get_block_template(long_poll_params(id)),
    )
    .await
    .expect("excess long poll at capacity must fail fast")
    .expect_err("excess long poll at capacity must return ServerIsBusy");
    assert_eq!(err.code(), ErrorCode::ServerIsBusy.code());
    assert_eq!(harness.rpc.gbt.active_long_polls(), expected_active);
}

async fn assert_non_parking_calls_succeed_at_capacity(
    harness: &LongPollHarness,
    template: &BlockTemplateResponse,
    cached: bool,
) {
    assert_excess_long_poll_rejected_immediately(harness, template.long_poll_id, 2).await;

    let immediate = harness
        .rpc
        .get_block_template(None)
        .await
        .expect("plain getblocktemplate must succeed while long polls are full")
        .try_into_template()
        .expect("template-mode response");
    assert_eq!(immediate.long_poll_id, template.long_poll_id);

    let mut stale_id = template.long_poll_id;
    stale_id.tip_hash_checksum ^= 1;
    let stale = harness
        .rpc
        .get_block_template(long_poll_params(stale_id))
        .await
        .expect("stale longpollid must return immediately without acquiring a permit")
        .try_into_template()
        .expect("template-mode response");
    assert_eq!(stale.long_poll_id, template.long_poll_id);
    assert_eq!(harness.rpc.gbt.active_long_polls(), 2, "cached: {cached}");
}

async fn assert_long_poll_limit_and_release(cached: bool) {
    let harness = LongPollHarness::new(2, 120);
    let template = harness.make_template(&harness.chain_info);
    if cached {
        harness.publish_template(&template);
    }

    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);
    let (lp1, lp2) = park_two_long_polls(&harness, template.long_poll_id).await;

    assert_non_parking_calls_succeed_at_capacity(&harness, &template, cached).await;

    drop(lp1);
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1, "cached: {cached}");

    let mut lp3: ParkedLongPollFuture<'_> = Box::pin(
        harness
            .rpc
            .get_block_template(long_poll_params(template.long_poll_id)),
    );
    assert!(futures::poll!(&mut lp3).is_pending(), "cached: {cached}");
    assert_eq!(harness.rpc.gbt.active_long_polls(), 2, "cached: {cached}");

    let replacement_info = harness.next_chain_info();
    let replacement = harness.make_template(&replacement_info);
    harness.advance_tip(&replacement_info);
    if cached {
        harness.publish_template(&replacement);
    }

    for lp in [lp2, lp3] {
        let response = tokio::time::timeout(Duration::from_secs(1), lp)
            .await
            .expect("tip change must wake parked long polls")
            .expect("parked long poll should return new template")
            .try_into_template()
            .expect("template-mode response");
        assert_eq!(response.long_poll_id, replacement.long_poll_id);
        assert_eq!(response.submit_old, Some(false));
    }

    assert_eq!(harness.rpc.gbt.active_long_polls(), 0, "cached: {cached}");
}

/// Long polls are bounded separately from general template requests, reject excess waits with
/// `ErrorCode::ServerIsBusy` (`-32009`), release permits when dropped or completed, and allow
/// regular `getblocktemplate` calls while at capacity.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_long_poll_respects_limit_and_releases_permits() {
    let _init_guard = zebra_test::init();

    for cached in [false, true] {
        assert_long_poll_limit_and_release(cached).await;
    }
}

async fn assert_long_poll_max_duration(cached: bool, already_expired: bool) {
    let offset = if already_expired {
        -10
    } else {
        i64::try_from(MAX_LONG_POLL_DURATION.as_secs()).unwrap() + 120
    };
    let harness = LongPollHarness::new(2, offset);
    let template = harness.make_template(&harness.chain_info);
    if cached {
        harness.publish_template(&template);
    }

    let waiting = harness
        .rpc
        .get_block_template(long_poll_params(template.long_poll_id));
    tokio::pin!(waiting);
    assert!(
        futures::poll!(&mut waiting).is_pending(),
        "cached: {cached}, already_expired: {already_expired}"
    );
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    tokio::time::advance(MAX_LONG_POLL_DURATION - Duration::from_secs(1)).await;
    assert!(
        futures::poll!(&mut waiting).is_pending(),
        "long poll must stay parked until MAX_LONG_POLL_DURATION elapses"
    );
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    let response = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("MAX_LONG_POLL_DURATION must end the long poll")
        .expect("expired long poll returns current template")
        .try_into_template()
        .expect("template-mode response");

    assert_eq!(response.long_poll_id, template.long_poll_id);
    assert_eq!(
        response.submit_old,
        Some(!already_expired),
        "cached: {cached}, already_expired: {already_expired}"
    );
    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);
}

/// A parked long poll returns the current template once `MAX_LONG_POLL_DURATION` elapses even
/// when neither the chain tip nor the mempool changes, including when `max_time` is not armed.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_long_poll_returns_after_max_duration() {
    let _init_guard = zebra_test::init();

    for cached in [false, true] {
        for already_expired in [false, true] {
            assert_long_poll_max_duration(cached, already_expired).await;
        }
    }
}

/// Concurrent long-poll requests waiting on precomputed template cache catch-up after a tip
/// advance must still respect the parked long-poll cap and release permits once served.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_long_poll_cache_catchup_respects_limit() {
    let _init_guard = zebra_test::init();

    let harness = LongPollHarness::new(2, 120);
    let old_template = harness.make_template(&harness.chain_info);
    harness.publish_template(&old_template);

    // Advance the committed state tip without publishing a new precomputed template yet so
    // `precomputed_template_for_state_tip_with_limit` waits on cache catch-up.
    let next_info = harness.next_chain_info();
    let next_template = harness.make_template(&next_info);
    harness.advance_tip(&next_info);

    let (lp1, lp2) = park_two_long_polls(&harness, old_template.long_poll_id).await;

    // A third long poll during cache catch-up must fail fast with ServerIsBusy (-32009).
    assert_excess_long_poll_rejected_immediately(&harness, old_template.long_poll_id, 2).await;

    // Non-long-poll calls still wait on cache catch-up without being blocked by the long-poll cap.
    let mut plain: ParkedLongPollFuture<'_> = Box::pin(harness.rpc.get_block_template(None));
    assert!(futures::poll!(&mut plain).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 2);

    // Publishing the new tip's template unblocks all waiters and releases both long-poll permits.
    harness.publish_template(&next_template);
    for fut in [lp1, lp2, plain] {
        let response = tokio::time::timeout(Duration::from_secs(1), fut)
            .await
            .expect("cache catch-up publish must unblock waiters")
            .expect("waiters must receive new tip template")
            .try_into_template()
            .expect("template-mode response");
        assert_eq!(response.long_poll_id, next_template.long_poll_id);
    }
    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);
}

async fn assert_stalled_refresh_times_out(stall_mempool: bool) {
    let offset = i64::try_from(MAX_LONG_POLL_DURATION.as_secs()).unwrap() + 120;
    let harness = LongPollHarness::new(2, offset);
    let template = harness.make_template(&harness.chain_info);

    let waiting = harness
        .rpc
        .get_block_template(long_poll_params(template.long_poll_id));
    tokio::pin!(waiting);
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    if stall_mempool {
        harness.stall_mempool.store(true, Ordering::SeqCst);
    } else {
        harness.stall_state_chain_info.store(true, Ordering::SeqCst);
    }

    // Trigger the periodic mempool refresh wake so the parked long poll enters the stalled read.
    tokio::time::advance(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)).await;
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    let remaining = MAX_LONG_POLL_DURATION - Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL);
    tokio::time::advance(remaining - Duration::from_secs(1)).await;
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    let err = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("MAX_LONG_POLL_DURATION must abort a stalled refresh read")
        .expect_err("stalled refresh must return a timeout error");
    assert_eq!(err.code(), ErrorCode::ServerIsBusy.code());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);
}

/// An already parked on-demand long poll whose state or mempool refresh read stalls must still
/// terminate at `MAX_LONG_POLL_DURATION` and release its permit.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_on_demand_long_poll_times_out_on_stalled_refresh() {
    let _init_guard = zebra_test::init();

    for stall_mempool in [false, true] {
        assert_stalled_refresh_times_out(stall_mempool).await;
    }
}

/// An already parked on-demand long poll encountering repeated state/mempool tip mismatches must
/// terminate at `MAX_LONG_POLL_DURATION` instead of looping past its deadline.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_on_demand_long_poll_times_out_on_repeated_tip_mismatches() {
    let _init_guard = zebra_test::init();

    let offset = i64::try_from(MAX_LONG_POLL_DURATION.as_secs()).unwrap() + 120;
    let harness = LongPollHarness::new(2, offset);
    let template = harness.make_template(&harness.chain_info);

    let waiting = harness
        .rpc
        .get_block_template(long_poll_params(template.long_poll_id));
    tokio::pin!(waiting);
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    // Make the mempool report a different tip hash than state on every refresh.
    harness.mempool_tip_tx.send_replace(Hash([0xff; 32]));

    let err = tokio::time::timeout(MAX_LONG_POLL_DURATION + Duration::from_secs(1), waiting)
        .await
        .expect("MAX_LONG_POLL_DURATION must stop repeated tip-mismatch retries")
        .expect_err("repeated tip mismatches past deadline must return a timeout error");
    assert_eq!(err.code(), ErrorCode::ServerIsBusy.code());
    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);
}

async fn assert_excess_long_poll_fails_fast_when_stalled(stall_mempool: bool) {
    let harness = LongPollHarness::new(2, 120);
    let template = harness.make_template(&harness.chain_info);

    let (_lp1, _lp2) = park_two_long_polls(&harness, template.long_poll_id).await;

    if stall_mempool {
        harness.stall_mempool.store(true, Ordering::SeqCst);
    } else {
        harness.stall_state_chain_info.store(true, Ordering::SeqCst);
    }

    assert_excess_long_poll_rejected_immediately(&harness, template.long_poll_id, 2).await;
}

/// When long-poll capacity is already exhausted, an incoming on-demand long poll whose ID
/// matches the current tip must fail fast even when initial state or mempool reads stall.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_excess_long_poll_fails_fast_when_initial_read_stalls() {
    let _init_guard = zebra_test::init();

    for stall_mempool in [false, true] {
        assert_excess_long_poll_fails_fast_when_stalled(stall_mempool).await;
    }
}

/// With a populated template cache and saturated long-poll capacity, an incoming matching long
/// poll must fail fast with `ServerIsBusy` before awaiting `ReadRequest::Tip`, while stale
/// long-poll IDs preserve committed-tip validation and return from cache once the tip read releases.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_cached_excess_long_poll_fails_fast_when_tip_read_stalls() {
    let _init_guard = zebra_test::init();

    let harness = LongPollHarness::new(2, 120);
    let template = harness.make_template(&harness.chain_info);
    harness.publish_template(&template);

    let (_lp1, _lp2) = park_two_long_polls(&harness, template.long_poll_id).await;

    harness.stall_tip_tx.send_replace(true);

    assert_excess_long_poll_rejected_immediately(&harness, template.long_poll_id, 2).await;

    let mut stale_id = template.long_poll_id;
    stale_id.tip_hash_checksum ^= 1;
    let mut stale = Box::pin(harness.rpc.get_block_template(long_poll_params(stale_id)));
    assert!(
        futures::poll!(&mut stale).is_pending(),
        "stale cached long poll must preserve committed-tip validation"
    );

    harness.stall_tip_tx.send_replace(false);
    let stale_response = tokio::time::timeout(Duration::from_millis(100), stale)
        .await
        .expect("stale cached long poll must complete before NEW_TIP_TIMEOUT once tip releases")
        .expect("stale cached long poll returns current template")
        .try_into_template()
        .expect("template-mode response");
    assert_eq!(stale_response.long_poll_id, template.long_poll_id);
    assert_eq!(
        harness.chain_info_calls.load(Ordering::SeqCst),
        0,
        "stale cached long poll should serve from cache without on-demand ChainInfo fallback"
    );
    assert_eq!(harness.rpc.gbt.active_long_polls(), 2);
}

async fn assert_long_poll_handles_yielding_tip_read_at_deadline(cached: bool) {
    let offset = i64::try_from(MAX_LONG_POLL_DURATION.as_secs()).unwrap() + 120;
    let harness = LongPollHarness::new(2, offset);
    let template = harness.make_template(&harness.chain_info);
    if cached {
        harness.publish_template(&template);
    }

    let waiting = harness
        .rpc
        .get_block_template(long_poll_params(template.long_poll_id));
    tokio::pin!(waiting);
    assert!(
        futures::poll!(&mut waiting).is_pending(),
        "cached: {cached}"
    );
    assert_eq!(harness.rpc.gbt.active_long_polls(), 1);

    harness.yield_tip.store(true, Ordering::SeqCst);
    tokio::time::advance(MAX_LONG_POLL_DURATION).await;

    let response = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("long poll must complete after MAX_LONG_POLL_DURATION")
        .expect("healthy yielding tip read at deadline must return current template")
        .try_into_template()
        .expect("template-mode response");
    assert_eq!(response.long_poll_id, template.long_poll_id);
    assert_eq!(response.submit_old, Some(true), "cached: {cached}");
    assert_eq!(harness.rpc.gbt.active_long_polls(), 0);

    // If the committed state tip advances right before deadline without updating the long poll,
    // preserve tip validation by rejecting with `ServerIsBusy` instead of returning stale work.
    let stale_harness = LongPollHarness::new(2, offset);
    let stale_template = stale_harness.make_template(&stale_harness.chain_info);
    if cached {
        stale_harness.publish_template(&stale_template);
    }

    let stale_waiting = stale_harness
        .rpc
        .get_block_template(long_poll_params(stale_template.long_poll_id));
    tokio::pin!(stale_waiting);
    assert!(
        futures::poll!(&mut stale_waiting).is_pending(),
        "cached: {cached}"
    );

    stale_harness
        .chain_info_tx
        .send_replace(stale_harness.next_chain_info());
    stale_harness.yield_tip.store(true, Ordering::SeqCst);
    tokio::time::advance(MAX_LONG_POLL_DURATION).await;

    let err = tokio::time::timeout(Duration::from_secs(1), stale_waiting)
        .await
        .expect("expired long poll must finish promptly")
        .expect_err("superseded template must not be returned after state tip advances");
    assert_eq!(err.code(), ErrorCode::ServerIsBusy.code());
    assert_eq!(stale_harness.rpc.gbt.active_long_polls(), 0);
}

/// Both cached and uncached parked long polls whose `ReadRequest::Tip` validation yields at
/// deadline still return the valid current template, while a committed tip advance before deadline
/// returns `ServerIsBusy`.
#[tokio::test(start_paused = true)]
async fn getblocktemplate_long_poll_handles_yielding_tip_read_at_deadline() {
    let _init_guard = zebra_test::init();

    for cached in [false, true] {
        assert_long_poll_handles_yielding_tip_read_at_deadline(cached).await;
    }
}
