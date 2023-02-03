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
        let expected_result = GetBlock::Raw(block.clone().into());

        let get_block = rpc
            .get_block(i.to_string(), Some(0u8))
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block, expected_result);

        let get_block = rpc
            .get_block(block.hash().to_string(), Some(0u8))
            .await
            .expect("We should have a GetBlock struct");

        assert_eq!(get_block, expected_result);
    }

    // Make calls with verbosity=1 and check response
    for (i, block) in blocks.iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), Some(1u8))
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

    for (i, block) in blocks.iter().enumerate() {
        let get_block = rpc
            .get_block(i.to_string(), None)
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
        .get_block("not parsable as height".to_string(), Some(0u8))
        .await
        .is_err());

    assert!(rpc
        .get_block("not parsable as height".to_string(), Some(1u8))
        .await
        .is_err());

    assert!(rpc
        .get_block("not parsable as height".to_string(), None)
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
    let block_future = tokio::spawn(rpc.get_block("0".to_string(), Some(0u8)));

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
        format!("invalid address \"{address}\": parse error: t-addr decoding error")
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
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

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
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
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
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

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
        MockAddressBookPeers::default(),
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
async fn rpc_getpeerinfo() {
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

    let _init_guard = zebra_test::init();
    let network = Mainnet;

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
        network,
        state.clone(),
        true,
    )
    .await;

    let mock_peer_address =
        zebra_network::types::MetaAddr::new_initial_peer(std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            network.default_port(),
        ))
        .into_new_meta_addr()
        .unwrap();

    let mock_address_book = MockAddressBookPeers::new(vec![mock_peer_address]);

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        network,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
        mock_address_book,
    );

    // Call `get_peer_info`
    let get_peer_info = get_block_template_rpc
        .get_peer_info()
        .await
        .expect("We should have an array of addresses");

    assert_eq!(
        get_peer_info
            .into_iter()
            .next()
            .expect("there should be a mock peer address"),
        mock_peer_address.into()
    );

    mempool.expect_no_requests().await;
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblockhash() {
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

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
        MockAddressBookPeers::default(),
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
async fn rpc_getmininginfo() {
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(MockService::build().for_unit_tests(), 1),
        read_state,
        latest_chain_tip.clone(),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
    );

    get_block_template_rpc
        .get_mining_info()
        .await
        .expect("get_mining_info call should succeed");
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getnetworksolps() {
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(MockService::build().for_unit_tests(), 1),
        read_state,
        latest_chain_tip.clone(),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
    );

    let get_network_sol_ps_inputs = [
        (None, None),
        (Some(0), None),
        (Some(0), Some(0)),
        (Some(0), Some(-1)),
        (Some(0), Some(10)),
        (Some(0), Some(i32::MAX)),
        (Some(1), None),
        (Some(1), Some(0)),
        (Some(1), Some(-1)),
        (Some(1), Some(10)),
        (Some(1), Some(i32::MAX)),
        (Some(usize::MAX), None),
        (Some(usize::MAX), Some(0)),
        (Some(usize::MAX), Some(-1)),
        (Some(usize::MAX), Some(10)),
        (Some(usize::MAX), Some(i32::MAX)),
    ];

    for (num_blocks_input, height_input) in get_network_sol_ps_inputs {
        let get_network_sol_ps_result = get_block_template_rpc
            .get_network_sol_ps(num_blocks_input, height_input)
            .await;
        assert!(
            get_network_sol_ps_result
                .is_ok(),
            "get_network_sol_ps({num_blocks_input:?}, {height_input:?}) call with should be ok, got: {get_network_sol_ps_result:?}"
        );
    }
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_getblocktemplate() {
    // test getblocktemplate with a miner P2SH address
    rpc_getblocktemplate_mining_address(true).await;
    // test getblocktemplate with a miner P2PKH address
    rpc_getblocktemplate_mining_address(false).await;
}

#[cfg(feature = "getblocktemplate-rpcs")]
async fn rpc_getblocktemplate_mining_address(use_p2pkh: bool) {
    use zebra_chain::{
        amount::NonNegative,
        block::{Hash, MAX_BLOCK_BYTES, ZCASH_BLOCK_VERSION},
        chain_sync_status::MockSyncStatus,
        chain_tip::mock::MockChainTip,
        serialization::DateTime32,
        work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
    };
    use zebra_consensus::MAX_BLOCK_SIGOPS;
    use zebra_network::address_book_peers::MockAddressBookPeers;
    use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};

    use crate::methods::{
        get_block_template_rpcs::{
            config::Config,
            constants::{
                GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD, GET_BLOCK_TEMPLATE_MUTABLE_FIELD,
                GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD,
            },
            get_block_template::{self, GetBlockTemplateRequestMode},
            types::{hex_data::HexData, long_poll::LONG_POLL_ID_LENGTH},
        },
        tests::utils::fake_history_tree,
    };

    let _init_guard = zebra_test::init();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();

    let mut read_state = MockService::build().for_unit_tests();
    let chain_verifier = MockService::build().for_unit_tests();

    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let miner_address = match use_p2pkh {
        false => Some(transparent::Address::from_script_hash(Mainnet, [0x7e; 20])),
        true => Some(transparent::Address::from_pub_key_hash(Mainnet, [0x7e; 20])),
    };

    let mining_config = Config { miner_address };

    // nu5 block height
    let fake_tip_height = NetworkUpgrade::Nu5.activation_height(Mainnet).unwrap();
    // nu5 block hash
    let fake_tip_hash =
        Hash::from_hex("0000000000d723156d9b65ffcf4984da7a19675ed7e2f06d9e5d5188af087bf8").unwrap();
    //  nu5 block time + 1
    let fake_min_time = DateTime32::from(1654008606);
    // nu5 block time + 12
    let fake_cur_time = DateTime32::from(1654008617);
    // nu5 block time + 123
    let fake_max_time = DateTime32::from(1654008728);
    let fake_difficulty = CompactDifficulty::from(ExpandedDifficulty::from(U256::one()));

    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();
    mock_chain_tip_sender.send_best_tip_height(fake_tip_height);
    mock_chain_tip_sender.send_best_tip_hash(fake_tip_hash);
    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));

    // Init RPC
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        Mainnet,
        mining_config,
        Buffer::new(mempool.clone(), 1),
        read_state.clone(),
        mock_chain_tip,
        chain_verifier,
        mock_sync_status.clone(),
        MockAddressBookPeers::default(),
    );

    // Fake the ChainInfo response
    let mock_read_state_request_handler = async move {
        read_state
            .expect_request_that(|req| matches!(req, ReadRequest::ChainInfo))
            .await
            .respond(ReadResponse::ChainInfo(GetBlockTemplateChainInfo {
                expected_difficulty: fake_difficulty,
                tip_height: fake_tip_height,
                tip_hash: fake_tip_hash,
                cur_time: fake_cur_time,
                min_time: fake_min_time,
                max_time: fake_max_time,
                history_tree: fake_history_tree(Mainnet),
            }));
    };

    let mock_mempool_request_handler = {
        let mut mempool = mempool.clone();
        async move {
            mempool
                .expect_request(mempool::Request::FullTransactions)
                .await
                .respond(mempool::Response::FullTransactions(vec![]));
        }
    };

    let get_block_template_fut = get_block_template_rpc.get_block_template(None);
    let (get_block_template, ..) = tokio::join!(
        get_block_template_fut,
        mock_mempool_request_handler,
        mock_read_state_request_handler,
    );

    let get_block_template::Response::TemplateMode(get_block_template) = get_block_template
        .expect("unexpected error in getblocktemplate RPC call") else {
            panic!("this getblocktemplate call without parameters should return the `TemplateMode` variant of the response")
        };

    assert_eq!(
        get_block_template.capabilities,
        GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD.to_vec()
    );
    assert_eq!(get_block_template.version, ZCASH_BLOCK_VERSION);
    assert!(get_block_template.transactions.is_empty());
    assert_eq!(
        get_block_template.target,
        ExpandedDifficulty::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001"
        )
        .expect("test vector is valid")
    );
    assert_eq!(get_block_template.min_time, fake_min_time);
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
    assert_eq!(get_block_template.cur_time, fake_cur_time);
    assert_eq!(
        get_block_template.bits,
        CompactDifficulty::from_hex("01010000").expect("test vector is valid")
    );
    assert_eq!(get_block_template.height, 1687105); // nu5 height
    assert_eq!(get_block_template.max_time, fake_max_time);

    // Coinbase transaction checks.
    assert!(get_block_template.coinbase_txn.required);
    assert!(!get_block_template.coinbase_txn.data.as_ref().is_empty());
    assert_eq!(get_block_template.coinbase_txn.depends.len(), 0);
    if use_p2pkh {
        // there is one sig operation if miner address is p2pkh.
        assert_eq!(get_block_template.coinbase_txn.sigops, 1);
    } else {
        // everything in the coinbase is p2sh.
        assert_eq!(get_block_template.coinbase_txn.sigops, 0);
    }
    // Coinbase transaction checks for empty blocks.
    assert_eq!(
        get_block_template.coinbase_txn.fee,
        Amount::<NonNegative>::zero()
    );

    mempool.expect_no_requests().await;

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(200));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(None)
        .await
        .expect_err("needs an error when estimated distance to network chain tip is far");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );

    mock_sync_status.set_is_close_to_tip(false);

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(None)
        .await
        .expect_err("needs an error when syncer is not close to tip");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );

    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(200));
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(None)
        .await
        .expect_err("needs an error when syncer is not close to tip or estimated distance to network chain tip is far");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );

    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(Some(get_block_template::JsonParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            ..Default::default()
        }))
        .await
        .expect_err("needs an error when called in proposal mode without data");

    assert_eq!(get_block_template_sync_error.code, ErrorCode::InvalidParams);

    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(Some(get_block_template::JsonParameters {
            data: Some(HexData("".into())),
            ..Default::default()
        }))
        .await
        .expect_err("needs an error when passing in block data in template mode");

    assert_eq!(get_block_template_sync_error.code, ErrorCode::InvalidParams);

    // The long poll id is valid, so it returns a state error instead
    let get_block_template_sync_error = get_block_template_rpc
        .get_block_template(Some(get_block_template::JsonParameters {
            // This must parse as a LongPollId.
            // It must be the correct length and have hex/decimal digits.
            long_poll_id: Some(
                "0".repeat(LONG_POLL_ID_LENGTH)
                    .parse()
                    .expect("unexpected invalid LongPollId"),
            ),
            ..Default::default()
        }))
        .await
        .expect_err("needs an error when the state is empty");

    assert_eq!(
        get_block_template_sync_error.code,
        ErrorCode::ServerError(-10)
    );
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_submitblock_errors() {
    use zebra_chain::chain_sync_status::MockSyncStatus;
    use zebra_network::address_book_peers::MockAddressBookPeers;

    use crate::methods::get_block_template_rpcs::types::{hex_data::HexData, submit_block};

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
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip.clone(),
        chain_verifier,
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
    );

    // Try to submit pre-populated blocks and assert that it responds with duplicate.
    for (_height, &block_bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.iter() {
        let submit_block_response = get_block_template_rpc
            .submit_block(HexData(block_bytes.into()), None)
            .await;

        assert_eq!(
            submit_block_response,
            Ok(submit_block::ErrorResponse::Duplicate.into())
        );
    }

    let submit_block_response = get_block_template_rpc
        .submit_block(
            HexData(zebra_test::vectors::BAD_BLOCK_MAINNET_202_BYTES.to_vec()),
            None,
        )
        .await;

    assert_eq!(
        submit_block_response,
        Ok(submit_block::ErrorResponse::Rejected.into())
    );

    mempool.expect_no_requests().await;

    // See zebrad::tests::acceptance::submit_block for success case.
}

#[cfg(feature = "getblocktemplate-rpcs")]
#[tokio::test(flavor = "multi_thread")]
async fn rpc_validateaddress() {
    use get_block_template_rpcs::types::validate_address;
    use zebra_chain::{chain_sync_status::MockSyncStatus, chain_tip::mock::MockChainTip};
    use zebra_network::address_book_peers::MockAddressBookPeers;

    let _init_guard = zebra_test::init();

    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();

    // Init RPC
    let get_block_template_rpc = get_block_template_rpcs::GetBlockTemplateRpcImpl::new(
        Mainnet,
        Default::default(),
        Buffer::new(MockService::build().for_unit_tests(), 1),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        MockAddressBookPeers::default(),
    );

    let validate_address = get_block_template_rpc
        .validate_address("t3fqvkzrrNaMcamkQMwAyHRjfDdM2xQvDTR".to_string())
        .await
        .expect("we should have a validate_address::Response");

    assert!(
        validate_address.is_valid,
        "Mainnet founder address should be valid on Mainnet"
    );

    let validate_address = get_block_template_rpc
        .validate_address("t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi".to_string())
        .await
        .expect("We should have a validate_address::Response");

    assert_eq!(
        validate_address,
        validate_address::Response::invalid(),
        "Testnet founder address should be invalid on Mainnet"
    );
}
