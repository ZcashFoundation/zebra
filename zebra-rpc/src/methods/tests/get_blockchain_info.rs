//! Unit tests for `getblockchaininfo` initial block download (IBD) completion and latching.

use std::time::Duration;

use tower::buffer::Buffer;

use zebra_chain::{
    block::{Hash, Height},
    chain_sync_status::{ChainSyncStatus, MockSyncStatus},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network::{self, Mainnet},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use super::super::*;

type TestMockService<Req, Resp> =
    MockService<Req, Resp, zebra_test::mock_service::PanicAssertion, BoxError>;

type TestRpcImpl = RpcImpl<
    TestMockService<zebra_node_services::mempool::Request, zebra_node_services::mempool::Response>,
    Buffer<TestMockService<zebra_state::Request, zebra_state::Response>, zebra_state::Request>,
    TestMockService<ReadRequest, ReadResponse>,
    MockChainTip,
    MockAddressBookPeers,
    TestMockService<zebra_consensus::Request, Hash>,
    MockSyncStatus,
>;

/// Invokes `getblockchaininfo` against a mock `read_state` handling `UsageInfo`,
/// `TipPoolValues`, and `BestChainNextDifficulty` in any order.
async fn fetch_blockchain_info(
    rpc: &TestRpcImpl,
    read_state: &mut TestMockService<ReadRequest, ReadResponse>,
    network: &Network,
    tip: Option<(Height, Hash)>,
) -> GetBlockchainInfoResponse {
    let state_handler = async {
        for _ in 0..3 {
            let sender = read_state.expect_request_that(|_| true).await;
            match sender.request() {
                ReadRequest::UsageInfo => sender.respond(ReadResponse::UsageInfo(0)),
                ReadRequest::TipPoolValues => match tip {
                    Some((tip_height, tip_hash)) => sender.respond(ReadResponse::TipPoolValues {
                        tip_height,
                        tip_hash,
                        value_balance: Default::default(),
                    }),
                    None => sender.respond(Err(BoxError::from("no chain tip available yet"))),
                },
                ReadRequest::BestChainNextDifficulty => {
                    let difficulty = tip.map(|_| network.target_difficulty_limit().into());
                    sender.respond(ReadResponse::BestChainNextDifficulty(difficulty))
                }
                other => panic!("unexpected read_state request: {other:?}"),
            }
        }
    };

    let (response, ()) = tokio::join!(rpc.get_blockchain_info(), state_handler);
    response.expect("getblockchaininfo should succeed")
}

/// Mock `RpcImpl` harness for `getblockchaininfo` IBD tests.
struct IbdTestHarness {
    network: Network,
    rpc: TestRpcImpl,
    read_state: TestMockService<ReadRequest, ReadResponse>,
    sync_status: MockSyncStatus,
    tip_sender: MockChainTipSender,
}

impl IbdTestHarness {
    fn new(network: Network, debug_force_finished_sync: bool) -> Self {
        let mempool: TestMockService<_, _> = MockService::build().for_unit_tests();
        let state: TestMockService<_, _> = MockService::build().for_unit_tests();
        let read_state: TestMockService<_, _> = MockService::build().for_unit_tests();
        let sync_status = MockSyncStatus::default();
        let (mock_tip, tip_sender) = MockChainTip::new();
        let (_tx, rx) = tokio::sync::watch::channel(None);

        let (rpc, _) = RpcImpl::new(
            network.clone(),
            Default::default(),
            debug_force_finished_sync,
            "0.0.1",
            "RPC test",
            mempool,
            Buffer::new(state, 1),
            read_state.clone(),
            MockService::build().for_unit_tests(),
            sync_status.clone(),
            mock_tip,
            MockAddressBookPeers::default(),
            rx,
            None,
        );

        Self {
            network,
            rpc,
            read_state,
            sync_status,
            tip_sender,
        }
    }

    fn send_tip(&self, height: Height, hash: Hash, block_time: chrono::DateTime<chrono::Utc>) {
        self.tip_sender.send_best_tip_height(height);
        self.tip_sender.send_best_tip_hash(hash);
        self.tip_sender.send_best_tip_block_time(block_time);
    }

    async fn fetch_info(&mut self, tip: Option<(Height, Hash)>) -> GetBlockchainInfoResponse {
        fetch_blockchain_info(&self.rpc, &mut self.read_state, &self.network, tip).await
    }

    async fn fetch_info_with(
        &mut self,
        rpc: &TestRpcImpl,
        tip: Option<(Height, Hash)>,
    ) -> GetBlockchainInfoResponse {
        fetch_blockchain_info(rpc, &mut self.read_state, &self.network, tip).await
    }
}

fn assert_ibd_incomplete(info: &GetBlockchainInfoResponse, tip_height: Height) {
    assert!(!info.initial_block_download_complete());
    assert!(info.estimated_height() > tip_height);
    assert!(info.verification_progress() < 1.0);
}

fn assert_ibd_complete(info: &GetBlockchainInfoResponse, tip_height: Height) {
    assert!(info.initial_block_download_complete());
    assert_eq!(info.blocks(), tip_height);
    assert_eq!(info.estimated_height(), tip_height);
    assert_eq!(info.verification_progress(), 1.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_mainnet_ibd_transition_and_latching() {
    let _init_guard = zebra_test::init();

    let max_checkpoint = Mainnet.checkpoint_list().max_height();
    let tip_hash = Hash([0x11; 32]);
    let mut harness = IbdTestHarness::new(Mainnet, false);
    let rpc_clone = harness.rpc.clone();
    let recent_tip = chrono::Utc::now() - chrono::Duration::minutes(10);

    // Below the highest checkpoint: IBD stays incomplete even when close to tip and recent.
    harness.sync_status.set_is_close_to_tip(true);
    let below_checkpoint = Height(10);
    harness.send_tip(below_checkpoint, tip_hash, recent_tip);
    let info = harness.fetch_info(Some((below_checkpoint, tip_hash))).await;
    assert_ibd_incomplete(&info, below_checkpoint);

    // At the highest checkpoint, but syncer is not close to tip yet.
    harness.sync_status.set_is_close_to_tip(false);
    harness.send_tip(max_checkpoint, tip_hash, recent_tip);
    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_incomplete(&info, max_checkpoint);

    // Close to tip and at checkpoint, but Mainnet tip is older than 125 minutes.
    harness.sync_status.set_is_close_to_tip(true);
    let stale_tip = chrono::Utc::now() - chrono::Duration::minutes(126);
    harness.send_tip(max_checkpoint, tip_hash, stale_tip);
    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_incomplete(&info, max_checkpoint);

    // Synced: at checkpoint, close to tip, and recent tip (10 min old -> would extrapolate >0 blocks).
    harness.send_tip(max_checkpoint, tip_hash, recent_tip);
    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_complete(&info, max_checkpoint);

    // Once latched, later syncer stalls or old tips do not revert IBD on `rpc` or clones.
    harness.sync_status.set_is_close_to_tip(false);
    harness
        .tip_sender
        .send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::hours(5));

    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_complete(&info, max_checkpoint);

    let clone_info = harness
        .fetch_info_with(&rpc_clone, Some((max_checkpoint, tip_hash)))
        .await;
    assert_ibd_complete(&clone_info, max_checkpoint);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_mainnet_ibd_latches_from_sync_without_prior_rpc_call() {
    let _init_guard = zebra_test::init();

    let max_checkpoint = Mainnet.checkpoint_list().max_height();
    let tip_hash = Hash([0x44; 32]);
    let mut harness = IbdTestHarness::new(Mainnet, false);

    // Mainnet finishes syncing without any `getblockchaininfo` RPC call.
    harness.sync_status.set_is_close_to_tip(true);
    harness.send_tip(
        max_checkpoint,
        tip_hash,
        chrono::Utc::now() - chrono::Duration::minutes(10),
    );

    // Wait for the background observer to latch IBD completion on its own.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !harness.rpc.is_ibd_latched() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("IBD observer should latch once Mainnet sync completes");

    // Tip then ages past 125 minutes and syncer falls behind before the first RPC call.
    harness.sync_status.set_is_close_to_tip(false);
    harness
        .tip_sender
        .send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::hours(5));
    assert!(!harness.sync_status.is_close_to_tip());

    // The very first `getblockchaininfo` call still reports IBD complete without extrapolating.
    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_complete(&info, max_checkpoint);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_testnet_ibd_respects_checkpoint_floor_without_tip_age_limit() {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let max_checkpoint = network.checkpoint_list().max_height();
    let tip_hash = Hash([0x22; 32]);
    let mut harness = IbdTestHarness::new(network, false);

    // Below the highest checkpoint on Testnet: stays in IBD even if close to tip.
    harness.sync_status.set_is_close_to_tip(true);
    let below_checkpoint = Height(10);
    harness.send_tip(
        below_checkpoint,
        tip_hash,
        chrono::Utc::now() - chrono::Duration::minutes(10),
    );
    let info = harness.fetch_info(Some((below_checkpoint, tip_hash))).await;
    assert_ibd_incomplete(&info, below_checkpoint);

    // At the highest checkpoint on Testnet: completes IBD even with a multi-day stale tip.
    harness.send_tip(
        max_checkpoint,
        tip_hash,
        chrono::Utc::now() - chrono::Duration::days(3),
    );
    let info = harness.fetch_info(Some((max_checkpoint, tip_hash))).await;
    assert_ibd_complete(&info, max_checkpoint);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_debug_force_empty_state_then_tip() {
    let _init_guard = zebra_test::init();

    // Empty state must not report IBD complete or latch, even with `debug_force_finished_sync`.
    let mut harness = IbdTestHarness::new(Mainnet, true);
    let info = harness.fetch_info(None).await;
    assert!(!info.initial_block_download_complete());

    let tip_height = Height(10);
    let tip_hash = Hash([0x33; 32]);
    harness.send_tip(
        tip_height,
        tip_hash,
        chrono::Utc::now() - chrono::Duration::days(10),
    );
    let info = harness.fetch_info(Some((tip_height, tip_hash))).await;
    assert_ibd_complete(&info, tip_height);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_regtest_empty_state_then_genesis() {
    let _init_guard = zebra_test::init();

    // Regtest requires a committed tip, then completes IBD even when `MockSyncStatus` is false.
    let network = Network::new_regtest(Default::default());
    let genesis_hash = network.genesis_hash();
    let mut harness = IbdTestHarness::new(network, false);

    let info = harness.fetch_info(None).await;
    assert!(!info.initial_block_download_complete());
    assert_eq!(info.verification_progress(), 1.0);

    harness.send_tip(
        Height(0),
        genesis_hash,
        chrono::Utc::now() - chrono::Duration::days(100),
    );
    let info = harness.fetch_info(Some((Height(0), genesis_hash))).await;
    assert_ibd_complete(&info, Height(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_mainnet_genesis_future_timestamp_is_not_nan() {
    let _init_guard = zebra_test::init();

    // Mainnet at genesis with a future timestamp during IBD must report `0.0` (not `NaN`).
    let genesis_hash = Mainnet.genesis_hash();
    let mut harness = IbdTestHarness::new(Mainnet, false);

    harness.send_tip(
        Height(0),
        genesis_hash,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    let info = harness.fetch_info(Some((Height(0), genesis_hash))).await;
    assert!(!info.initial_block_download_complete());
    assert_eq!(info.estimated_height(), Height(0));
    assert_eq!(info.verification_progress(), 0.0);

    let json = serde_json::to_string(&info).expect("response serializes without NaN/null");
    let parsed: GetBlockchainInfoResponse =
        serde_json::from_str(&json).expect("serialized response parses back cleanly");
    assert_eq!(parsed, info);
}
