//! Tests for `getaddresstxids` `chainInfo` responses and atomic snapshot retry behavior.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use tower::buffer::Buffer;

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
use zebra_test::mock_service::MockService;

use super::super::*;

fn expect_chain_info(response: GetAddressTxIdsResponse) -> GetAddressTxIdsResponseObject {
    match response {
        GetAddressTxIdsResponse::TxIdsAndChainInfo(obj) => obj,
        other => panic!("expected GetAddressTxIdsResponse::TxIdsAndChainInfo, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_chain_info() {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let tip_block = blocks.last().unwrap();
    let tip_block_hash = tip_block.hash();
    let tip_block_height = tip_block.coinbase_height().unwrap();

    let address = blocks[1].transactions[0].outputs()[1]
        .address(&Mainnet)
        .unwrap();
    let expected_all_txids: Vec<String> = blocks[1..]
        .iter()
        .map(|block| block.transactions[0].hash().to_string())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks.clone(), &Mainnet).await;

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

    let addresses = vec![address.to_string()];

    // `chainInfo: false` returns the bare array variant.
    let response = rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), None, None).with_chain_info(false),
        )
        .await
        .expect("chainInfo=false should succeed");
    let GetAddressTxIdsResponse::TxIds(txids) = response else {
        panic!("expected GetAddressTxIdsResponse::TxIds variant");
    };
    assert_eq!(txids, expected_all_txids);

    // `chainInfo: true` returns `{ txids, hash, height }` for the full range.
    let obj = expect_chain_info(
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), None, None).with_chain_info(true),
        )
        .await
        .expect("chainInfo=true should succeed"),
    );
    assert_eq!(obj.txids(), &expected_all_txids);
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    // `start: 0, end: 0` with `chainInfo: true` matches the full range and reports the tip.
    let obj = expect_chain_info(
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), Some(0), Some(0)).with_chain_info(true),
        )
        .await
        .expect("start=0, end=0 with chainInfo=true should succeed"),
    );
    assert_eq!(obj.txids(), &expected_all_txids);
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    // Sub-range queries still report the chain snapshot tip, not `end`.
    let obj = expect_chain_info(
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), Some(2), Some(5)).with_chain_info(true),
        )
        .await
        .expect("sub-range with chainInfo=true should succeed"),
    );
    assert_eq!(obj.txids(), &expected_all_txids[1..5]);
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    // Clamped `start`/`end` above the tip still return the tip block's txids and tip metadata.
    let obj = expect_chain_info(
        rpc.get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), Some(15), Some(20))
                .with_chain_info(true),
        )
        .await
        .expect("clamped range above tip with chainInfo=true should succeed"),
    );
    assert_eq!(obj.txids(), &[expected_all_txids[9].clone()]);
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    // Unused valid address with `chainInfo: true` returns empty `txids` and the snapshot tip.
    let obj = expect_chain_info(
        rpc.get_address_tx_ids(
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
    assert!(obj.txids().is_empty());
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    // `MAX_ADDRESS_QUERY_RESULTS` and `MAX_REQUEST_ADDRESSES` apply when `chainInfo: true`.
    let capped_rpc = rpc.clone().with_max_address_query_results(3);
    let invalid_params = i32::from(server::error::LegacyCode::InvalidParameter);
    let obj = expect_chain_info(
        capped_rpc
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(addresses.clone(), Some(1), Some(3))
                    .with_chain_info(true),
            )
            .await
            .expect("query at cap should succeed"),
    );
    assert_eq!(obj.txids().len(), 3);
    assert_eq!(obj.hash(), tip_block_hash);
    assert_eq!(obj.height(), tip_block_height);

    let over_cap = capped_rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), Some(1), Some(4)).with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(over_cap.code(), invalid_params);

    let too_many_addresses = vec![addresses[0].clone(); MAX_REQUEST_ADDRESSES + 1];
    let over_addr_cap = capped_rpc
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(too_many_addresses, None, None).with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(over_addr_cap.code(), invalid_params);

    mempool.expect_no_requests().await;
    queue.abort();

    // Empty state / NoChainTip: address validation runs before tip lookup, and `chainInfo: true`
    // rejects empty state snapshots with `"No blocks in state"`.
    let (_tx_empty, rx_empty) = tokio::sync::watch::channel(None);
    let (rpc_no_tip, queue_no_tip) = RpcImpl::new(
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
        rx_empty,
        None,
    );
    let invalid_addr_err = rpc_no_tip
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
    let no_blocks_err = rpc_no_tip
        .get_address_tx_ids(
            GetAddressTxIdsRequest::new(addresses.clone(), None, None).with_chain_info(true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        no_blocks_err.code(),
        i32::from(server::error::LegacyCode::Misc)
    );
    assert!(no_blocks_err.message().contains("No blocks in state"));
    queue_no_tip.abort();

    let (mock_tip, mock_tip_sender) = MockChainTip::new();
    mock_tip_sender.send_best_tip_height(Height(0));
    let mut mock_read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (_tx_mock, rx_mock) = tokio::sync::watch::channel(None);
    let (rpc_empty_snapshot, queue_empty_snapshot) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        Buffer::new(mock_read_state.clone(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        mock_tip,
        MockAddressBookPeers::default(),
        rx_mock,
        None,
    );
    let call = tokio::spawn(async move {
        rpc_empty_snapshot
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(addresses, None, None).with_chain_info(true),
            )
            .await
    });
    mock_read_state
        .expect_request_that(|req| matches!(req, ReadRequest::TransactionIdsByAddresses { .. }))
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: Default::default(),
            tip: None,
        });
    let empty_tip_err = call
        .await
        .expect("task should not panic")
        .expect_err("empty snapshot tip with chainInfo=true must error");
    assert_eq!(
        empty_tip_err.code(),
        i32::from(server::error::LegacyCode::Misc)
    );
    assert!(empty_tip_err.message().contains("No blocks in state"));
    queue_empty_snapshot.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_atomic_snapshot_and_clamping_retry() {
    let _init_guard = zebra_test::init();

    let addr_str = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string();
    let addr_set: HashSet<zebra_chain::transparent::Address> =
        HashSet::from([addr_str.parse().expect("valid address")]);

    let (mock_tip, mock_tip_sender) = MockChainTip::new();
    mock_tip_sender.send_best_tip_height(Height(10));

    let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, queue) = RpcImpl::new(
        Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC test",
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        MockService::build().for_unit_tests::<_, _, BoxError>(),
        Buffer::new(read_state.clone(), 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        mock_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );
    let rpc = rpc.with_max_address_query_results(2);

    let hash_8 = Hash([8u8; 32]);
    let hash_11 = Hash([11u8; 32]);
    let tx_a = TxHash::from([1u8; 32]);
    let tx_b = TxHash::from([2u8; 32]);
    let tx_c = TxHash::from([3u8; 32]);
    let tx_8 = TxHash::from([8u8; 32]);
    let tx_11 = TxHash::from([11u8; 32]);

    // 1. Unbounded range while tip advances 10 -> 11: succeeds in a single call with 0 retries.
    let rpc_clone = rpc.clone();
    let addr_clone = addr_str.clone();
    let call = tokio::spawn(async move {
        rpc_clone
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(vec![addr_clone], None, None).with_chain_info(true),
            )
            .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: addr_set.clone(),
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
    assert_eq!(res.txids(), &[tx_11.to_string()]);
    assert_eq!(res.height(), Height(11));
    assert_eq!(res.hash(), hash_11);

    // 2. Clamped `start > tip` (`start=11, end=11`, tip `10 -> 11`), where stale block 10 has
    // `> cap` txids: range mismatch is detected *before* cap rejection and retried at 11..=11.
    let rpc_clone = rpc.clone();
    let addr_clone = addr_str.clone();
    let call = tokio::spawn(async move {
        rpc_clone
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(vec![addr_clone], Some(11), Some(11))
                    .with_chain_info(true),
            )
            .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: addr_set.clone(),
            height_range: Height(10)..=Height(10),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::from([
                (TransactionLocation::from_usize(Height(10), 0), tx_a),
                (TransactionLocation::from_usize(Height(10), 1), tx_b),
                (TransactionLocation::from_usize(Height(10), 2), tx_c),
            ]),
            tip: Some((Height(11), hash_11)),
        });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: addr_set.clone(),
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
    assert_eq!(res.txids(), &[tx_11.to_string()]);
    assert_eq!(res.height(), Height(11));
    assert_eq!(res.hash(), hash_11);

    // 3. `start=15, end=10` clamped at tip 10 to `10..=10`, then snapshot tip moves to 11:
    // re-evaluating `build_height_range(..., 11)` returns `InvalidParams` without retrying.
    let rpc_clone = rpc.clone();
    let addr_clone = addr_str.clone();
    let call = tokio::spawn(async move {
        rpc_clone
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(vec![addr_clone], Some(15), Some(10))
                    .with_chain_info(true),
            )
            .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: addr_set.clone(),
            height_range: Height(10)..=Height(10),
            max_entries: Some(3),
        })
        .await
        .respond(ReadResponse::AddressesTransactionIds {
            tx_ids: BTreeMap::new(),
            tip: Some((Height(11), hash_11)),
        });
    read_state.expect_no_requests().await;
    let err = call
        .await
        .expect("task should not panic")
        .expect_err("start > end at snapshot tip must return InvalidParams");
    assert_eq!(err.code(), ErrorCode::InvalidParams.code());

    // 4. Reorg shortening tip (`10 -> 8` with `start=9, end=None`): state returns empty at
    // snapshot height 8, RPC detects `9 != 8`, retries with `8..=8`, and returns block 8's tip.
    let rpc_clone = rpc.clone();
    let addr_clone = addr_str;
    let call = tokio::spawn(async move {
        rpc_clone
            .get_address_tx_ids(
                GetAddressTxIdsRequest::new(vec![addr_clone], Some(9), None).with_chain_info(true),
            )
            .await
    });
    read_state
        .expect_request(ReadRequest::TransactionIdsByAddresses {
            addresses: addr_set.clone(),
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
            addresses: addr_set,
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
    assert_eq!(res.txids(), &[tx_8.to_string()]);
    assert_eq!(res.height(), Height(8));
    assert_eq!(res.hash(), hash_8);

    queue.abort();
}
