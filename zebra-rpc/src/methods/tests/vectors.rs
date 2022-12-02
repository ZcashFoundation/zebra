//! Fixed test vectors for RPC methods.

use std::{ops::RangeInclusive, sync::Arc};

use jsonrpc_core::ErrorCode;
use tower::buffer::Buffer;

use zebra_chain::{
    amount::Amount,
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

#[cfg(feature = "getblocktemplate-rpcs")]
use zebra_chain::chain_sync_status::MockSyncStatus;

#[tokio::test(flavor = "multi_thread")]
async fn rpc_getinfo() {
    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
    );

    // Get the tip hash using RPC method `get_best_block_hash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBlockHash struct");
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
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
                        conventional_fee: Amount::zero(),
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
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
        "start Height(2) must be less than or equal to end Height(1)".to_string()
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
        "start Height(0) and end Height(1) must both be greater than zero".to_string()
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
        "start Height(1) and end Height(11) must both be less than or equal to the chain tip Height(10)".to_string()
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
        network,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(state.clone(), 1),
        NoChainTip,
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
        Mainnet,
        false,
        Buffer::new(mempool.clone(), 1),
        Buffer::new(read_state.clone(), 1),
        latest_chain_tip,
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

#[tokio::test(flavor = "multi_thread")]
#[cfg(feature = "getblocktemplate-rpcs")]
async fn rpc_getblockcount() {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Get the height of the block at the tip using hardcoded block tip bytes.
    // We want to test the RPC response is equal to this hash
    let tip_block = blocks.last().unwrap();
    let tip_block_height = tip_block.coinbase_height().unwrap();

    // Get a mempool handle
    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service, the tip will be in `NUMBER_OF_BLOCKS`.
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    let (
        chain_verifier,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::chain::init(
        zebra_consensus::Config::default(),
        Mainnet,
        state.clone(),
        true,
    )
    .await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
    );

    // Get the tip height using RPC method `get_block_count`
    let get_block_count = get_block_template_rpc
        .get_block_count()
        .expect("We should have a number");

    // Check if response is equal to block 10 hash.
    assert_eq!(get_block_count, tip_block_height.0);

    mempool.expect_no_requests().await;
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockcount_empty_state() {
    let _init_guard = zebra_test::init();

    // Get a mempool handle
    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create an empty state
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(Mainnet);

    let (
        chain_verifier,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::chain::init(
        zebra_consensus::Config::default(),
        Mainnet,
        state.clone(),
        true,
    )
    .await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
    );

    // Get the tip height using RPC method `get_block_count
    let get_block_count = get_block_template_rpc.get_block_count();

    // state an empty so we should get an error
    assert!(get_block_count.is_err());

    // Check the error we got is the correct one
    assert_eq!(get_block_count.err().unwrap().message, "No blocks in state");

    mempool.expect_no_requests().await;
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockhash() {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    let (
        chain_verifier,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::chain::init(
        zebra_consensus::Config::default(),
        Mainnet,
        state.clone(),
        true,
    )
    .await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        tower::ServiceBuilder::new().service(chain_verifier),
        MockSyncStatus::default(),
    );

    // Query the hashes using positive indexes
    for (i, block) in blocks.iter().enumerate() {
        let get_block_hash = get_block_template_rpc
            .get_block_hash(i.try_into().expect("usize always fits in i32"))
            .await
            .expect("We should have a GetBlockHash struct");

        assert_eq!(get_block_hash, GetBlockHash(block.clone().hash()));
    }

    // Query the hashes using negative indexes
    for i in (-10..=-1).rev() {
        let get_block_hash = get_block_template_rpc
            .get_block_hash(i)
            .await
            .expect("We should have a GetBlockHash struct");

        assert_eq!(
            get_block_hash,
            GetBlockHash(blocks[(10 + (i + 1)) as usize].hash())
        );
    }

    mempool.expect_no_requests().await;
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblocktemplate() {
    use std::panic;

    use chrono::{TimeZone, Utc};

    use crate::methods::{
        get_block_template_rpcs::constants::{
            GET_BLOCK_TEMPLATE_MUTABLE_FIELD, GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD,
        },
        tests::utils::fake_history_tree,
    };
    use zebra_chain::{
        amount::{Amount, NonNegative},
        block::{Hash, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION},
        chain_tip::mock::MockChainTip,
        work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
    };
    use zebra_consensus::MAX_BLOCK_SIGOPS;

    use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};

    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let read_state = MockService::build().for_unit_tests();
    let chain_verifier = MockService::build().for_unit_tests();

    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let mining_config = get_block_template_rpcs::config::Config {
        miner_address: Some(transparent::Address::from_script_hash(Mainnet, [0x7e; 20])),
    };

    // nu5 block height
    let fake_tip_height = NetworkUpgrade::Nu5.activation_height(Mainnet).unwrap();
    // nu5 block hash
    let fake_tip_hash =
        Hash::from_hex("0000000000d723156d9b65ffcf4984da7a19675ed7e2f06d9e5d5188af087bf8").unwrap();
    //  nu5 block time + 1
    let fake_min_time = Utc.timestamp_opt(1654008606, 0).unwrap();
    // nu5 block time + 12
    let fake_cur_time = Utc.timestamp_opt(1654008617, 0).unwrap();
    // nu5 block time + 123
    let fake_max_time = Utc.timestamp_opt(1654008728, 0).unwrap();

    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();
    mock_chain_tip_sender.send_best_tip_height(fake_tip_height);
    mock_chain_tip_sender.send_best_tip_hash(fake_tip_hash);
    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        mining_config,
        Buffer::new(mempool.clone(), 1),
        read_state.clone(),
        mock_chain_tip,
        chain_verifier,
        mock_sync_status.clone(),
    );

    // Fake the ChainInfo response
    tokio::spawn(async move {
        read_state
            .clone()
            .expect_request_that(|req| matches!(req, ReadRequest::ChainInfo))
            .await
            .respond(ReadResponse::ChainInfo(Some(GetBlockTemplateChainInfo {
                expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
                tip: (fake_tip_height, fake_tip_hash),
                cur_time: fake_cur_time,
                min_time: fake_min_time,
                max_time: fake_max_time,
                history_tree: fake_history_tree(Mainnet),
            })));
    });

    let get_block_template = tokio::spawn(get_block_template_rpc.get_block_template());

    mempool
        .expect_request(mempool::Request::FullTransactions)
        .await
        .respond(mempool::Response::FullTransactions(vec![]));

    let get_block_template = get_block_template
        .await
        .unwrap_or_else(|error| match error.try_into_panic() {
            Ok(panic_object) => panic::resume_unwind(panic_object),
            Err(cancelled_error) => {
                panic!("getblocktemplate task was unexpectedly cancelled: {cancelled_error:?}")
            }
        })
        .expect("unexpected error in getblocktemplate RPC call");

    assert_eq!(get_block_template.capabilities, Vec::<String>::new());
    assert_eq!(get_block_template.version, ZCASH_BLOCK_VERSION);
    assert!(get_block_template.transactions.is_empty());
    assert_eq!(
        get_block_template.target,
        "0000000000000000000000000000000000000000000000000000000000000001"
    );
    assert_eq!(get_block_template.min_time, fake_min_time.timestamp());
    assert_eq!(
        get_block_template.mutable,
        GET_BLOCK_TEMPLATE_MUTABLE_FIELD.to_vec()
    );
    assert_eq!(
        get_block_template.nonce_range,
        GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD
    );
    assert_eq!(get_block_template.sigop_limit, MAX_BLOCK_SIGOPS);
    assert_eq!(get_block_template.size_limit, MAX_BLOCK_BYTES);
    assert_eq!(get_block_template.cur_time, fake_cur_time.timestamp());
    assert_eq!(get_block_template.bits, "01010000");
    assert_eq!(get_block_template.height, 1687105); // nu5 height
    assert_eq!(get_block_template.max_time, fake_max_time.timestamp());

    // Coinbase transaction checks.
    assert!(get_block_template.coinbase_txn.required);
    assert!(!get_block_template.coinbase_txn.data.as_ref().is_empty());
    assert_eq!(get_block_template.coinbase_txn.depends.len(), 0);
    // TODO: should a coinbase transaction have sigops?
    assert_eq!(get_block_template.coinbase_txn.sigops, 0);
    // Coinbase transaction checks for empty blocks.
    assert_eq!(
        get_block_template.coinbase_txn.fee,
        Amount::<NonNegative>::zero()
    );

    mempool.expect_no_requests().await;

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(200));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template()
        .await
        .expect_err("needs an error when estimated distance to network chain tip is far");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );

    mock_sync_status.set_is_close_to_tip(false);

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template()
        .await
        .expect_err("needs an error when syncer is not close to tip");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(200));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template()
        .await
        .expect_err("needs an error when syncer is not close to tip or estimated distance to network chain tip is far");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_submitblock_errors() {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks, Mainnet).await;

    // Init RPCs
    let (
        chain_verifier,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::chain::init(
        zebra_consensus::Config::default(),
        Mainnet,
        state.clone(),
        true,
    )
    .await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
    );

    // Try to submit pre-populated blocks and assert that it responds with duplicate.
    for (_height, &block_bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.iter() {
        let submit_block_response = get_block_template_rpc
            .submit_block(
                get_block_template_rpcs::types::hex_data::HexData(block_bytes.into()),
                None,
            )
            .await;

        assert_eq!(
            submit_block_response,
            Ok(get_block_template_rpcs::types::submit_block::ErrorResponse::Duplicate.into())
        );
    }

    let submit_block_response = get_block_template_rpc
        .submit_block(
            get_block_template_rpcs::types::hex_data::HexData(
                zebra_test::vectors::BAD_BLOCK_MAINNET_202_BYTES.to_vec(),
            ),
            None,
        )
        .await;

    assert_eq!(
        submit_block_response,
        Ok(get_block_template_rpcs::types::submit_block::ErrorResponse::Rejected.into())
    );

    mempool.expect_no_requests().await;

    // See zebrad::tests::acceptance::submit_block for success case.
}
