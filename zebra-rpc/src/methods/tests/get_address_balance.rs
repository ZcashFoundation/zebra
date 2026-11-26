//! Tests for `getaddressbalance` `chainInfo` requests and responses.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    amount::Amount,
    block::{Block, Hash},
    chain_sync_status::MockSyncStatus,
    chain_tip::NoChainTip,
    parameters::Network::Mainnet,
    serialization::ZcashDeserializeInto,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::super::*;

const TEST_ADDR: &str = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd";
const UNUSED_ADDR: &str = "t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak";

type UnitMock<Req, Resp> = MockService<Req, Resp, PanicAssertion, BoxError>;
type MockMempool = UnitMock<mempool::Request, mempool::Response>;
type MockBlockVerifier = UnitMock<zebra_consensus::Request, Hash>;

type MockRpc = RpcImpl<
    MockMempool,
    UnitMock<zebra_state::Request, zebra_state::Response>,
    Buffer<UnitMock<ReadRequest, ReadResponse>, ReadRequest>,
    NoChainTip,
    MockAddressBookPeers,
    MockBlockVerifier,
    MockSyncStatus,
>;

struct PopulatedFixture {
    rpc: RpcImpl<
        Buffer<MockMempool, mempool::Request>,
        Buffer<
            BoxService<zebra_state::Request, zebra_state::Response, BoxError>,
            zebra_state::Request,
        >,
        Buffer<zebra_state::ReadStateService, ReadRequest>,
        zebra_state::LatestChainTip,
        MockAddressBookPeers,
        MockBlockVerifier,
        MockSyncStatus,
    >,
    mempool: MockMempool,
    queue: JoinHandle<()>,
    addresses: Vec<String>,
    expected_balance: u64,
    tip_hash: Hash,
    tip_height: Height,
}

async fn populated_fixture() -> PopulatedFixture {
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let tip_block = blocks.last().unwrap();
    let tip_hash = tip_block.hash();
    let tip_height = tip_block.coinbase_height().unwrap();
    let address = blocks[1].transactions[0].outputs()[1]
        .address(&Mainnet)
        .unwrap();
    let expected_balance: u64 = blocks[1..]
        .iter()
        .map(|block| u64::from(block.transactions[0].outputs()[1].value))
        .sum();

    let mempool = MockService::build().for_unit_tests();
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks, &Mainnet).await;
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        state,
        Buffer::new(read_state, 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    PopulatedFixture {
        rpc,
        mempool,
        queue,
        addresses: vec![address.to_string()],
        expected_balance,
        tip_hash,
        tip_height,
    }
}

fn mock_read_state_rpc() -> (MockRpc, UnitMock<ReadRequest, ReadResponse>, JoinHandle<()>) {
    let read_state = MockService::build().for_unit_tests();
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        Buffer::new(read_state.clone(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );
    (rpc, read_state, queue)
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressbalance_chain_info_populated() {
    let _init_guard = zebra_test::init();
    let mut f = populated_fixture().await;

    let legacy = f
        .rpc
        .get_address_balance(GetAddressBalanceRequest::new(f.addresses.clone()))
        .await
        .expect("chainInfo=false should succeed");
    assert_eq!(
        legacy,
        GetAddressBalanceResponse::new(f.expected_balance, f.expected_balance)
    );
    assert_eq!(legacy.hash(), None);
    assert_eq!(legacy.height(), None);

    let with_ci = f
        .rpc
        .get_address_balance(GetAddressBalanceRequest::new(f.addresses).with_chain_info(true))
        .await
        .expect("chainInfo=true should succeed");
    assert_eq!(
        with_ci,
        GetAddressBalanceResponse::new(f.expected_balance, f.expected_balance)
            .with_chain_info(f.tip_hash, f.tip_height)
    );

    f.mempool.expect_no_requests().await;
    f.queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressbalance_chain_info_unused_address() {
    let _init_guard = zebra_test::init();
    let mut f = populated_fixture().await;

    let resp = f
        .rpc
        .get_address_balance(
            GetAddressBalanceRequest::new(vec![UNUSED_ADDR.to_string()]).with_chain_info(true),
        )
        .await
        .expect("unused address with chainInfo=true should succeed");

    assert_eq!(
        resp,
        GetAddressBalanceResponse::new(0, 0).with_chain_info(f.tip_hash, f.tip_height)
    );

    f.mempool.expect_no_requests().await;
    f.queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressbalance_chain_info_empty_state_errors() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_read_state_rpc();

    let call = tokio::spawn(async move {
        rpc.get_address_balance(
            GetAddressBalanceRequest::new(vec![TEST_ADDR.to_string()]).with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request_that(|req| matches!(req, ReadRequest::AddressBalance(_)))
        .await
        .respond(ReadResponse::AddressBalance {
            balance: Amount::zero(),
            received: 0,
            tip: None,
        });

    let err = call
        .await
        .expect("task should not panic")
        .expect_err("empty snapshot tip with chainInfo=true must error");
    assert_eq!(err.code(), i32::from(server::error::LegacyCode::Misc));
    assert!(err.message().contains("No blocks in state"));
    queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressbalance_chain_info_validates_addresses_first() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_read_state_rpc();

    let invalid_err = rpc
        .get_address_balance(
            GetAddressBalanceRequest::new(vec!["t1invalid".to_string()]).with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        invalid_err.code(),
        i32::from(server::error::LegacyCode::InvalidAddressOrKey)
    );

    let too_many = vec![TEST_ADDR.to_string(); MAX_REQUEST_ADDRESSES + 1];
    let cap_err = rpc
        .get_address_balance(GetAddressBalanceRequest::new(too_many).with_chain_info(true))
        .await
        .unwrap_err();
    assert_eq!(
        cap_err.code(),
        i32::from(server::error::LegacyCode::InvalidParameter)
    );

    read_state.expect_no_requests().await;
    queue.abort();
}

#[test]
fn get_address_balance_request_deserialization() {
    let single: GetAddressBalanceRequest =
        serde_json::from_str(&format!(r#""{TEST_ADDR}""#)).expect("single string should parse");
    assert_eq!(
        single,
        GetAddressBalanceRequest::new(vec![TEST_ADDR.to_string()])
    );
    assert!(!single.chain_info());

    let default_obj: GetAddressBalanceRequest =
        serde_json::from_str(&format!(r#"{{"addresses":["{TEST_ADDR}"]}}"#))
            .expect("object without chainInfo should parse");
    assert!(!default_obj.chain_info());

    let with_ci: GetAddressBalanceRequest = serde_json::from_str(&format!(
        r#"{{"addresses":["{TEST_ADDR}"],"chainInfo":true}}"#
    ))
    .expect("object with chainInfo=true should parse");
    assert!(with_ci.chain_info());
}
