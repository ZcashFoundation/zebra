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
async fn drive_get_blockchain_info<E: std::fmt::Debug>(
    fut: impl std::future::Future<Output = std::result::Result<GetBlockchainInfoResponse, E>>,
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

    let (response, ()) = tokio::join!(fut, state_handler);
    response.expect("getblockchaininfo should succeed")
}

/// Creates an `RpcImpl` wired with mock services for `getblockchaininfo` tests.
fn test_get_blockchain_info_rpc(
    network: Network,
    debug_force_finished_sync: bool,
) -> (
    TestRpcImpl,
    TestMockService<ReadRequest, ReadResponse>,
    MockSyncStatus,
    MockChainTipSender,
) {
    let mempool: TestMockService<_, _> = MockService::build().for_unit_tests();
    let state: TestMockService<_, _> = MockService::build().for_unit_tests();
    let read_state: TestMockService<_, _> = MockService::build().for_unit_tests();
    let sync_status = MockSyncStatus::default();
    let (mock_tip, mock_tip_sender) = MockChainTip::new();
    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network,
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

    (rpc, read_state, sync_status, mock_tip_sender)
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

    let network = Mainnet;
    let max_checkpoint = network.checkpoint_list().max_height();
    let tip_hash = Hash([0x11; 32]);
    let (rpc, mut read_state, mut sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), false);
    let rpc_clone = rpc.clone();

    // Below the highest checkpoint: IBD stays incomplete even when close to tip and recent.
    sync_status.set_is_close_to_tip(true);
    let below_checkpoint = Height(10);
    tip_sender.send_best_tip_height(below_checkpoint);
    tip_sender.send_best_tip_hash(tip_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(10));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((below_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_incomplete(&info, below_checkpoint);

    // At the highest checkpoint, but syncer is not close to tip yet.
    sync_status.set_is_close_to_tip(false);
    tip_sender.send_best_tip_height(max_checkpoint);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(10));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((max_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_incomplete(&info, max_checkpoint);

    // Close to tip and at checkpoint, but Mainnet tip is older than 125 minutes.
    sync_status.set_is_close_to_tip(true);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(126));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((max_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_incomplete(&info, max_checkpoint);

    // Synced: at checkpoint, close to tip, and recent tip (10 min old -> would extrapolate >0 blocks).
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(10));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((max_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_complete(&info, max_checkpoint);

    // Once latched, later syncer stalls or old tips do not revert IBD on `rpc` or clones.
    sync_status.set_is_close_to_tip(false);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::hours(5));

    for client in [&rpc, &rpc_clone] {
        let info = drive_get_blockchain_info(
            client.get_blockchain_info(),
            &mut read_state,
            &network,
            Some((max_checkpoint, tip_hash)),
        )
        .await;
        assert_ibd_complete(&info, max_checkpoint);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_mainnet_ibd_latches_from_sync_without_prior_rpc_call() {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let max_checkpoint = network.checkpoint_list().max_height();
    let tip_hash = Hash([0x44; 32]);
    let (rpc, mut read_state, mut sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), false);

    // Mainnet finishes syncing without any `getblockchaininfo` RPC call.
    sync_status.set_is_close_to_tip(true);
    tip_sender.send_best_tip_height(max_checkpoint);
    tip_sender.send_best_tip_hash(tip_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(10));

    // Wait for the background observer to latch IBD completion on its own.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !rpc.is_ibd_latched() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("IBD observer should latch once Mainnet sync completes");

    // Tip then ages past 125 minutes and syncer falls behind before the first RPC call.
    sync_status.set_is_close_to_tip(false);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::hours(5));
    assert!(!sync_status.is_close_to_tip());

    // The very first `getblockchaininfo` call still reports IBD complete without extrapolating.
    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((max_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_complete(&info, max_checkpoint);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_testnet_ibd_respects_checkpoint_floor_without_tip_age_limit() {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let max_checkpoint = network.checkpoint_list().max_height();
    let tip_hash = Hash([0x22; 32]);
    let (rpc, mut read_state, mut sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), false);

    // Below the highest checkpoint on Testnet: stays in IBD even if close to tip.
    sync_status.set_is_close_to_tip(true);
    let below_checkpoint = Height(10);
    tip_sender.send_best_tip_height(below_checkpoint);
    tip_sender.send_best_tip_hash(tip_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::minutes(10));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((below_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_incomplete(&info, below_checkpoint);

    // At the highest checkpoint on Testnet: completes IBD even with a multi-day stale tip.
    tip_sender.send_best_tip_height(max_checkpoint);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::days(3));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((max_checkpoint, tip_hash)),
    )
    .await;
    assert_ibd_complete(&info, max_checkpoint);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_debug_force_empty_state_then_tip() {
    let _init_guard = zebra_test::init();

    // Empty state must not report IBD complete or latch, even with `debug_force_finished_sync`.
    let network = Mainnet;
    let (rpc, mut read_state, _sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), true);

    let info =
        drive_get_blockchain_info(rpc.get_blockchain_info(), &mut read_state, &network, None).await;
    assert!(!info.initial_block_download_complete());

    let tip_height = Height(10);
    let tip_hash = Hash([0x33; 32]);
    tip_sender.send_best_tip_height(tip_height);
    tip_sender.send_best_tip_hash(tip_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::days(10));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((tip_height, tip_hash)),
    )
    .await;
    assert_ibd_complete(&info, tip_height);
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_regtest_empty_state_then_genesis() {
    let _init_guard = zebra_test::init();

    // Regtest requires a committed tip, then completes IBD even when `MockSyncStatus` is false.
    let network = Network::new_regtest(Default::default());
    let genesis_hash = network.genesis_hash();
    let (rpc, mut read_state, _sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), false);

    let info =
        drive_get_blockchain_info(rpc.get_blockchain_info(), &mut read_state, &network, None).await;
    assert!(!info.initial_block_download_complete());
    assert_eq!(info.verification_progress(), 1.0);

    tip_sender.send_best_tip_height(Height(0));
    tip_sender.send_best_tip_hash(genesis_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() - chrono::Duration::days(100));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((Height(0), genesis_hash)),
    )
    .await;
    assert_ibd_complete(&info, Height(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockchaininfo_mainnet_genesis_future_timestamp_is_not_nan() {
    let _init_guard = zebra_test::init();

    // Mainnet at genesis with a future timestamp during IBD must report `0.0` (not `NaN`).
    let network = Mainnet;
    let genesis_hash = network.genesis_hash();
    let (rpc, mut read_state, _sync_status, tip_sender) =
        test_get_blockchain_info_rpc(network.clone(), false);

    tip_sender.send_best_tip_height(Height(0));
    tip_sender.send_best_tip_hash(genesis_hash);
    tip_sender.send_best_tip_block_time(chrono::Utc::now() + chrono::Duration::minutes(5));

    let info = drive_get_blockchain_info(
        rpc.get_blockchain_info(),
        &mut read_state,
        &network,
        Some((Height(0), genesis_hash)),
    )
    .await;
    assert!(!info.initial_block_download_complete());
    assert_eq!(info.estimated_height(), Height(0));
    assert_eq!(info.verification_progress(), 0.0);

    let json = serde_json::to_string(&info).expect("response serializes without NaN/null");
    let parsed: GetBlockchainInfoResponse =
        serde_json::from_str(&json).expect("serialized response parses back cleanly");
    assert_eq!(parsed, info);
}
