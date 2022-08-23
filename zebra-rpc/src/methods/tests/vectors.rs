//! Fixed test vectors for RPC methods.

use std::{ops::RangeInclusive, sync::Arc};

use jsonrpc_core::ErrorCode;
use tower::buffer::Buffer;

use zebra_chain::{
    block::Block,
    chain_tip::NoChainTip,
    parameters::Network::*,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{UnminedTx, UnminedTxId},
    transparent,
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::BoxError;

use zebra_test::mock_service::MockService;

use super::super::*;

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getinfo() {
    let _init_guard = zebra_test::init();

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
    // and that is equal to the provided string, with an added 'v' version prefix.
    assert_eq!(get_info.build, "vRPC test");

    // make sure there is a `subversion` field,
    // and that is equal to the Zebra user agent.
    assert_eq!(get_info.subversion, USER_AGENT);

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblock() {
    let _init_guard = zebra_test::init();

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

    // Make calls with verbosity=0 and check response
    for (i, block) in blocks.iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), 0u8)
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block, GetBlock::Raw(block.clone().into()));
    }

    // Make calls with verbosity=1 and check response
    for (i, block) in blocks.iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), 1u8)
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(
            get_block,
            GetBlock::Object {
                tx: block
                    .transactions
                    .iter()
                    .map(|tx| tx.hash().encode_hex())
                    .collect()
            }
        );
    }

    mempool.expect_no_requests().await;

    // The queue task should continue without errors or panics
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(matches!(rpc_tx_queue_task_result, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblock_parse_error() {
    let _init_guard = zebra_test::init();

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

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblock_missing_error() {
    let _init_guard = zebra_test::init();

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

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getbestblockhash() {
    let _init_guard = zebra_test::init();

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

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getrawtransaction() {
    let _init_guard = zebra_test::init();

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

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_invalid_arguments() {
    let _init_guard = zebra_test::init();

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
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start,
            end,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "invalid address \"{}\": parse error: t-addr decoding error",
            address.clone()
        )
    );

    // create a valid address
    let address = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".to_string();
    let addresses = vec![address.clone()];

    // call the method with start greater than end
    let start: u32 = 2;
    let end: u32 = 1;
    let error = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start,
            end,
        })
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
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start,
            end,
        })
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
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses,
            start,
            end,
        })
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

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddresstxids_response() {
    let _init_guard = zebra_test::init();

    for network in [Mainnet, Testnet] {
        let blocks: Vec<Arc<Block>> = match network {
            Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
            Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
        }
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

        // The first few blocks after genesis send funds to the same founders reward address,
        // in one output per coinbase transaction.
        //
        // Get the coinbase transaction of the first block
        // (the genesis block coinbase transaction is ignored by the consensus rules).
        let first_block_first_transaction = &blocks[1].transactions[0];

        // Get the address.
        let address = first_block_first_transaction.outputs()[1]
            .address(network)
            .unwrap();

        if network == Mainnet {
            // Exhaustively test possible block ranges for mainnet.
            //
            // TODO: if it takes too long on slower machines, turn this into a proptest with 10-20 cases
            for start in 1..=10 {
                for end in start..=10 {
                    rpc_getaddresstxids_response_with(network, start..=end, &blocks, &address)
                        .await;
                }
            }
        } else {
            // Just test the full range for testnet.
            rpc_getaddresstxids_response_with(network, 1..=10, &blocks, &address).await;
        }
    }
}

async fn rpc_getaddresstxids_response_with(
    network: Network,
    range: RangeInclusive<u32>,
    blocks: &[Arc<Block>],
    address: &transparent::Address,
) {
    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.to_owned(), network).await;

    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
        network,
    );

    // call the method with valid arguments
    let addresses = vec![address.to_string()];
    let response = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses,
            start: *range.start(),
            end: *range.end(),
        })
        .await
        .expect("arguments are valid so no error can happen here");

    // One founders reward output per coinbase transactions, no other transactions.
    assert_eq!(response.len(), range.count());

    mempool.expect_no_requests().await;

    // Shut down the queue task, to close the state's file descriptors.
    // (If we don't, opening ~100 simultaneous states causes process file descriptor limit errors.)
    //
    // TODO: abort all the join handles in all the tests, except one?
    rpc_tx_queue_task_handle.abort();

    // The queue task should not have panicked or exited by itself.
    // It can still be running, or it can have exited due to the abort.
    let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
    assert!(
        rpc_tx_queue_task_result.is_none()
            || rpc_tx_queue_task_result
                .unwrap()
                .unwrap_err()
                .is_cancelled()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressutxos_invalid_arguments() {
    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
        Mainnet,
    );

    // call the method with an invalid address string
    let address = "11111111".to_string();
    let addresses = vec![address.clone()];
    let error = rpc
        .0
        .get_address_utxos(AddressStrings::new(addresses))
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "invalid address \"{}\": parse error: t-addr decoding error",
            address
        )
    );

    mempool.expect_no_requests().await;
    state.expect_no_requests().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getaddressutxos_response() {
    let _init_guard = zebra_test::init();

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

    let rpc = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
        Mainnet,
    );

    // call the method with a valid address
    let addresses = vec![address.to_string()];
    let response = rpc
        .0
        .get_address_utxos(AddressStrings::new(addresses))
        .await
        .expect("address is valid so no error can happen here");

    // there are 10 outputs for provided address
    assert_eq!(response.len(), 10);

    mempool.expect_no_requests().await;
}
