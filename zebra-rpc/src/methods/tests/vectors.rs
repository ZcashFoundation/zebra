//! Fixed test vectors for RPC methods.

use std::sync::Arc;

use jsonrpc_core::ErrorCode;
use tower::buffer::Buffer;

use zebra_chain::{
    block::Block,
    chain_tip::NoChainTip,
    parameters::Network::*,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::BoxError;

use zebra_test::mock_service::MockService;

use super::super::*;

#[tokio::test]
async fn rpc_getinfo() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    let get_info = rpc.get_info().expect("We should have a GetInfo struct");

    // make sure there is a `build` field in the response,
    // and that is equal to the provided string.
    assert_eq!(get_info.build, "RPC test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getblock() {
    zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Make calls and check response
    for (i, block) in blocks.into_iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), 0u8)
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block.0, block.into());
    }

    mempool.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getblock_parse_error() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    // Init RPC
    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    // Make sure we get an error if Zebra can't parse the block height.
    assert!(rpc
        .get_block("not parsable as height".to_string(), 0u8)
        .await
        .is_err());

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getblock_missing_error() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    // Init RPC
    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    // Make sure Zebra returns the correct error code `-8` for missing blocks
    // https://github.com/adityapk00/lightwalletd/blob/c1bab818a683e4de69cd952317000f9bb2932274/common/common.go#L251-L254
    let block_future = tokio::spawn(rpc.get_block("0".to_string(), 0u8));

    // Make the mock service respond with no block
    let response_handler = state
        .expect_request(zebra_state::ReadRequest::Block(Height(0).into()))
        .await;
    response_handler.respond(zebra_state::ReadResponse::Block(None));

    let block_response = block_future.await;
    let block_response = block_response
        .expect("unexpected panic in spawned request future")
        .expect_err("unexpected success from missing block state response");
    assert_eq!(block_response.code, ErrorCode::ServerError(-8),);

    // Now check the error string the way `lightwalletd` checks it
    assert_eq!(
        serde_json::to_string(&block_response)
            .expect("unexpected error serializing JSON error")
            .split(':')
            .nth(1)
            .expect("unexpectedly low number of error fields")
            .split(',')
            .next(),
        Some("-8")
    );

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getbestblockhash() {
    zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Get the hash of the block at the tip using hardcoded block tip bytes.
    // We want to test the RPC response is equal to this hash
    let tip_block = blocks.last().unwrap();
    let tip_block_hash = tip_block.hash();

    // Get a mempool handle
    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service, the tip will be in `NUMBER_OF_BLOCKS`.
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Get the tip hash using RPC method `get_best_block_hash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBestBlockHash struct");
    let response_hash = get_best_block_hash.0;

    // Check if response is equal to block 10 hash.
    assert_eq!(response_hash, tip_block_hash);

    mempool.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getrawtransaction() {
    zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Test case where transaction is in mempool.
    // Skip genesis because its tx is not indexed.
    for block in blocks.iter().skip(1) {
        for tx in block.transactions.iter() {
            let mempool_req = mempool
                .expect_request_that(|request| {
                    if let mempool::Request::TransactionsByMinedId(ids) = request {
                        ids.len() == 1 && ids.contains(&tx.hash())
                    } else {
                        false
                    }
                })
                .map(|responder| {
                    responder.respond(mempool::Response::Transactions(vec![UnminedTx {
                        id: UnminedTxId::Legacy(tx.hash()),
                        transaction: tx.clone(),
                        size: 0,
                    }]));
                });
            let get_tx_req = rpc.get_raw_transaction(tx.hash().encode_hex(), 0u8);
            let (response, _) = futures::join!(get_tx_req, mempool_req);
            let get_tx = response.expect("We should have a GetRawTransaction struct");
            if let GetRawTransaction::Raw(raw_tx) = get_tx {
                assert_eq!(raw_tx.as_ref(), tx.zcash_serialize_to_vec().unwrap());
            } else {
                unreachable!("Should return a Raw enum")
            }
        }
    }

    // Test case where transaction is _not_ in mempool.
    // Skip genesis because its tx is not indexed.
    for block in blocks.iter().skip(1) {
        for tx in block.transactions.iter() {
            let mempool_req = mempool
                .expect_request_that(|request| {
                    if let mempool::Request::TransactionsByMinedId(ids) = request {
                        ids.len() == 1 && ids.contains(&tx.hash())
                    } else {
                        false
                    }
                })
                .map(|responder| {
                    responder.respond(mempool::Response::Transactions(vec![]));
                });
            let get_tx_req = rpc.get_raw_transaction(tx.hash().encode_hex(), 0u8);
            let (response, _) = futures::join!(get_tx_req, mempool_req);
            let get_tx = response.expect("We should have a GetRawTransaction struct");
            if let GetRawTransaction::Raw(raw_tx) = get_tx {
                assert_eq!(raw_tx.as_ref(), tx.zcash_serialize_to_vec().unwrap());
            } else {
                unreachable!("Should return a Raw enum")
            }
        }
    }

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getaddresstxids_invalid_arguments() {
    zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
        Mainnet,
    );

    // call the method with an invalid address string
    let address = "11111111".to_string();
    let addresses = vec![address.clone()];
    let start: u32 = 1;
    let end: u32 = 2;
    let error = rpc
        .get_address_tx_ids(addresses, start, end)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        format!("Provided address is not valid: {}", address)
    );

    // create a valid address
    let address = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string();
    let addresses = vec![address.clone()];

    // call the method with start greater than end
    let start: u32 = 2;
    let end: u32 = 1;
    let error = rpc
        .get_address_tx_ids(addresses.clone(), start, end)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "End value is expected to be greater than or equal to start".to_string()
    );

    // call the method with start equal zero
    let start: u32 = 0;
    let end: u32 = 1;
    let error = rpc
        .get_address_tx_ids(addresses.clone(), start, end)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "Start and end are expected to be greater than zero".to_string()
    );

    // call the method outside the chain tip height
    let start: u32 = 1;
    let end: u32 = 11;
    let error = rpc
        .get_address_tx_ids(addresses, start, end)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "Start or end is outside chain range".to_string()
    );

    mempool.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test]
async fn rpc_getaddresstxids_response() {
    zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // get the first transaction of the first block
    let first_block_first_transaction = &blocks[1].transactions[0];
    // get the address, this is always `t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd`
    let address = &first_block_first_transaction.outputs()[1]
        .address(Mainnet)
        .unwrap();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
        Mainnet,
    );

    // call the method with valid arguments
    let addresses = vec![address.to_string()];
    let start: u32 = 1;
    let end: u32 = 1;
    let response = rpc
        .get_address_tx_ids(addresses, start, end)
        .await
        .expect("arguments are valid so no error can happen here");

    // TODO: The length of the response should be 1
    // Fix in the context of #3147
    assert_eq!(response.len(), 10);

    mempool.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}
