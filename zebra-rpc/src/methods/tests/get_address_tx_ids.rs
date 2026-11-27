//! Tests for `getaddresstxids` `chainInfo` responses and atomic snapshot retry behavior.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use tokio::task::JoinHandle;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    block::{Block, Hash},
    chain_sync_status::MockSyncStatus,
    chain_tip::{mock::MockChainTip, NoChainTip},
    parameters::Network::Mainnet,
    serialization::ZcashDeserializeInto,
    transaction::Hash as TxHash,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_state::{ReadRequest, ReadResponse, TransactionLocation};
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::super::*;

const TEST_ADDR: &str = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd";

type UnitMock<Req, Resp> = MockService<Req, Resp, PanicAssertion, BoxError>;
type MockMempool = UnitMock<mempool::Request, mempool::Response>;
type MockBlockVerifier = UnitMock<zebra_consensus::Request, Hash>;

type MockRpc<Tip> = RpcImpl<
    MockMempool,
    UnitMock<zebra_state::Request, zebra_state::Response>,
    Buffer<UnitMock<ReadRequest, ReadResponse>, ReadRequest>,
    Tip,
    MockAddressBookPeers,
    MockBlockVerifier,
    MockSyncStatus,
>;

fn expect_chain_info(response: GetAddressTxIdsResponse) -> GetAddressTxIdsResponseObject {
    match response {
        GetAddressTxIdsResponse::TxIdsAndChainInfo(obj) => obj,
        other => panic!("expected GetAddressTxIdsResponse::TxIdsAndChainInfo, got {other:?}"),
    }
}

fn assert_chain_info(
    obj: &GetAddressTxIdsResponseObject,
    expected_txids: &[String],
    hash: Hash,
    height: Height,
) {
    assert_eq!(obj.txids(), expected_txids);
    assert_eq!(obj.hash(), hash);
    assert_eq!(obj.height(), height);
}

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
    expected_all_txids: Vec<String>,
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
    let expected_all_txids = blocks[1..]
        .iter()
        .map(|block| block.transactions[0].hash().to_string())
        .collect();

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
        expected_all_txids,
        tip_hash,
        tip_height,
    }
}

fn mock_tip_rpc(
    height: Height,
) -> (
    MockRpc<MockChainTip>,
    UnitMock<ReadRequest, ReadResponse>,
    JoinHandle<()>,
) {
    let (mock_tip, mock_tip_sender) = MockChainTip::new();
    mock_tip_sender.send_best_tip_height(height);
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
        mock_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );
    (rpc, read_state, queue)
}

fn test_addr_set() -> HashSet<zebra_chain::transparent::Address> {
    HashSet::from([TEST_ADDR.parse().expect("valid address")])
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info_ranges() {
    let _init_guard = zebra_test::init();
    let mut f = populated_fixture().await;

    let legacy = f
        .rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(f.addresses.clone(), None, None).with_chain_info(false),
        )
        .await
        .expect("chainInfo=false should succeed");
    assert_eq!(
        legacy,
        GetAddressTxIdsResponse::TxIds(f.expected_all_txids.clone())
    );

    let full = expect_chain_info(
        f.rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(f.addresses.clone(), None, None).with_chain_info(true),
            )
            .await
            .expect("chainInfo=true should succeed"),
    );
    assert_chain_info(&full, &f.expected_all_txids, f.tip_hash, f.tip_height);

    let zero_range = expect_chain_info(
        f.rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(f.addresses.clone(), Some(0), Some(0))
                    .with_chain_info(true),
            )
            .await
            .expect("start=0, end=0 with chainInfo=true should succeed"),
    );
    assert_chain_info(&zero_range, &f.expected_all_txids, f.tip_hash, f.tip_height);

    let sub_range = expect_chain_info(
        f.rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(f.addresses, Some(2), Some(5)).with_chain_info(true),
            )
            .await
            .expect("sub-range with chainInfo=true should succeed"),
    );
    assert_chain_info(
        &sub_range,
        &f.expected_all_txids[1..5],
        f.tip_hash,
        f.tip_height,
    );

    f.mempool.expect_no_requests().await;
    f.queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info_clamped_and_unused() {
    let _init_guard = zebra_test::init();
    let mut f = populated_fixture().await;

    let clamped = expect_chain_info(
        f.rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(f.addresses, Some(15), Some(20)).with_chain_info(true),
            )
            .await
            .expect("clamped range above tip with chainInfo=true should succeed"),
    );
    assert_chain_info(
        &clamped,
        &f.expected_all_txids[9..10],
        f.tip_hash,
        f.tip_height,
    );

    let unused = expect_chain_info(
        f.rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(
                    vec!["t1at7nVNsv6taLRrNRvnQdtfLNRDfsGc3Ak".to_string()],
                    None,
                    None,
                )
                .with_chain_info(true),
            )
            .await
            .expect("unused address with chainInfo=true should succeed"),
    );
    assert_chain_info(&unused, &[], f.tip_hash, f.tip_height);

    f.mempool.expect_no_requests().await;
    f.queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info_caps() {
    let _init_guard = zebra_test::init();
    let mut f = populated_fixture().await;
    let capped_rpc = f.rpc.with_max_address_query_results(3);
    let invalid_params = i32::from(server::error::LegacyCode::InvalidParameter);

    let at_cap = expect_chain_info(
        capped_rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(f.addresses.clone(), Some(1), Some(3))
                    .with_chain_info(true),
            )
            .await
            .expect("query at cap should succeed"),
    );
    assert_chain_info(
        &at_cap,
        &f.expected_all_txids[0..3],
        f.tip_hash,
        f.tip_height,
    );

    let over_cap = capped_rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(f.addresses.clone(), Some(1), Some(4))
                .with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(over_cap.code(), invalid_params);

    let too_many = vec![f.addresses[0].clone(); MAX_REQUEST_ADDRESSES + 1];
    let over_addr_cap = capped_rpc
        .get_address_tx_ids(GetAddressTxIdsRequest::new(too_many, None, None).with_chain_info(true))
        .await
        .unwrap_err();
    assert_eq!(over_addr_cap.code(), invalid_params);

    f.mempool.expect_no_requests().await;
    f.queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info_no_chain_tip() {
    let _init_guard = zebra_test::init();
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        NoChainTip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let invalid_addr_err = rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec!["t1invalid".to_string()], None, None)
                .with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        invalid_addr_err.code(),
        i32::from(server::error::LegacyCode::InvalidAddressOrKey)
    );

    let no_blocks_err = rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], None, None)
                .with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        no_blocks_err.code(),
        i32::from(server::error::LegacyCode::Misc)
    );
    assert!(no_blocks_err.message().contains("No blocks in state"));
    queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info_empty_snapshot_tip() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_tip_rpc(Height(0));

    let call = tokio::spawn(async move {
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], None, None)
                .with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request_that(|req| matches!(req, ReadRequest::TransactionIdsByAddresses { .. }))
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: Default::default(),
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
async fn rpc_getaddresstxids_unbounded_tip_advance_single_pass() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_tip_rpc(Height(10));
    let rpc = rpc.with_max_address_query_results(2);
    let hash_11 = Hash([11u8; 32]);
    let tx_11 = TxHash::from([11u8; 32]);

    let call = tokio::spawn(async move {
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], None, None)
                .with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(0)..=Height::MAX,
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::from([(TransactionLocation::from_usize(Height(11), 0), tx_11)]),
            tip: Some((Height(11), hash_11)),
        });
    read_state.expect_no_requests().await;

    let res = expect_chain_info(
        call.await
            .expect("task should not panic")
            .expect("unbounded call should succeed in one pass"),
    );
    assert_chain_info(&res, &[tx_11.to_string()], hash_11, Height(11));
    queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_clamped_start_retries_before_cap() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_tip_rpc(Height(10));
    let rpc = rpc.with_max_address_query_results(2);
    let hash_11 = Hash([11u8; 32]);
    let tx_11 = TxHash::from([11u8; 32]);

    let call = tokio::spawn(async move {
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], Some(11), Some(11))
                .with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(10)..=Height(10),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::from([
                (
                    TransactionLocation::from_usize(Height(10), 0),
                    TxHash::from([1u8; 32]),
                ),
                (
                    TransactionLocation::from_usize(Height(10), 1),
                    TxHash::from([2u8; 32]),
                ),
                (
                    TransactionLocation::from_usize(Height(10), 2),
                    TxHash::from([3u8; 32]),
                ),
            ]),
            tip: Some((Height(11), hash_11)),
        });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(11)..=Height(11),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::from([(TransactionLocation::from_usize(Height(11), 0), tx_11)]),
            tip: Some((Height(11), hash_11)),
        });

    let res = expect_chain_info(
        call.await
            .expect("task should not panic")
            .expect("retry at updated snapshot tip should succeed"),
    );
    assert_chain_info(&res, &[tx_11.to_string()], hash_11, Height(11));
    queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_invalid_range_at_updated_tip_no_retry() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_tip_rpc(Height(10));
    let rpc = rpc.with_max_address_query_results(2);

    let call = tokio::spawn(async move {
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], Some(15), Some(10))
                .with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(10)..=Height(10),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::new(),
            tip: Some((Height(11), Hash([11u8; 32]))),
        });
    read_state.expect_no_requests().await;

    let err = call
        .await
        .expect("task should not panic")
        .expect_err("start > end at snapshot tip must return InvalidParams");
    assert_eq!(err.code(), ErrorCode::InvalidParams.code());
    queue.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_reorg_shortening_tip_retries() {
    let _init_guard = zebra_test::init();
    let (rpc, mut read_state, queue) = mock_tip_rpc(Height(10));
    let rpc = rpc.with_max_address_query_results(2);
    let hash_8 = Hash([8u8; 32]);
    let tx_8 = TxHash::from([8u8; 32]);

    let call = tokio::spawn(async move {
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(vec![TEST_ADDR.to_string()], Some(9), None)
                .with_chain_info(true),
        )
        .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(9)..=Height::MAX,
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::new(),
            tip: Some((Height(8), hash_8)),
        });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: test_addr_set(),
            height_range: Height(8)..=Height(8),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::from([(TransactionLocation::from_usize(Height(8), 0), tx_8)]),
            tip: Some((Height(8), hash_8)),
        });

    let res = expect_chain_info(
        call.await
            .expect("task should not panic")
            .expect("reorg retry should succeed at shortened tip"),
    );
    assert_chain_info(&res, &[tx_8.to_string()], hash_8, Height(8));
    queue.abort();
}
