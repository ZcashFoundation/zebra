//! Snapshot tests for Zebra JSON-RPC responses.

use std::sync::Arc;

use insta::dynamic_redaction;

use zebra_chain::{
    block::Block,
    parameters::Network::{Mainnet, Testnet},
    serialization::ZcashDeserializeInto,
};
use zebra_network::constants::USER_AGENT;
use zebra_node_services::BoxError;
use zebra_test::mock_service::MockService;

use super::super::*;

/// Snapshot test for RPC methods responses.
#[tokio::test(flavor = "multi_thread")]
async fn test_rpc_response_data() {
    let _init_guard = zebra_test::init();

    test_rpc_response_data_for_network(Mainnet).await;
    test_rpc_response_data_for_network(Testnet).await;
}

async fn test_rpc_response_data_for_network(network: Network) {
    // Create a continuous chain of mainnet and testnet blocks from genesis
    let block_data = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    let blocks: Vec<Arc<Block>> = block_data
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), network).await;

    // Init RPC
    let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        network,
    );

    // Start snapshots of RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_{}", network_string(network), blocks.len() - 1));

    // `getinfo`
    let get_info = rpc.get_info().expect("We should have a GetInfo struct");
    snapshot_rpc_getinfo(get_info, &settings);

    // `getblockchaininfo`
    let get_blockchain_info = rpc
        .get_blockchain_info()
        .expect("We should have a GetBlockChainInfo struct");
    snapshot_rpc_getblockchaininfo(get_blockchain_info, &settings);

    // get the first transaction of the first block which is not the genesis
    let first_block_first_transaction = &blocks[1].transactions[0];

    // build addresses
    let address = &first_block_first_transaction.outputs()[1]
        .address(network)
        .unwrap();
    let addresses = vec![address.to_string()];

    // `getaddressbalance`
    let get_address_balance = rpc
        .get_address_balance(AddressStrings {
            addresses: addresses.clone(),
        })
        .await
        .expect("We should have an AddressBalance struct");
    snapshot_rpc_getaddressbalance(get_address_balance, &settings);

    // `getblock`, verbosity=0
    const BLOCK_HEIGHT: u32 = 1;
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), 0u8)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock(get_block, block_data.get(&BLOCK_HEIGHT).unwrap(), &settings);

    // `getblock`, verbosity=1
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), 1u8)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose(get_block, &settings);

    // `getbestblockhash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBestBlockHash struct");
    snapshot_rpc_getbestblockhash(get_best_block_hash, &settings);

    // `getrawmempool`
    //
    // - a request to get all mempool transactions will be made by `getrawmempool` behind the scenes.
    // - as we have the mempool mocked we need to expect a request and wait for a response,
    // which will be an empty mempool in this case.
    let mempool_req = mempool
        .expect_request_that(|_request| true)
        .map(|responder| {
            responder.respond(mempool::Response::TransactionIds(
                std::collections::HashSet::new(),
            ));
        });

    // make the api call
    let get_raw_mempool = rpc.get_raw_mempool();
    let (response, _) = futures::join!(get_raw_mempool, mempool_req);
    let get_raw_mempool = response.expect("We should have a GetRawTransaction struct");

    snapshot_rpc_getrawmempool(get_raw_mempool, &settings);

    // `z_gettreestate`
    let tree_state = rpc
        .z_get_treestate(BLOCK_HEIGHT.to_string())
        .await
        .expect("We should have a GetTreestate struct");
    snapshot_rpc_z_gettreestate(tree_state, &settings);

    // `getrawtransaction`
    //
    // - similar to `getrawmempool` described above, a mempool request will be made to get the requested
    // transaction from the mempoo, response will be empty as we have this transaction in state
    let mempool_req = mempool
        .expect_request_that(|_request| true)
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    // make the api call
    let get_raw_transaction =
        rpc.get_raw_transaction(first_block_first_transaction.hash().encode_hex(), 0u8);
    let (response, _) = futures::join!(get_raw_transaction, mempool_req);
    let get_raw_transaction = response.expect("We should have a GetRawTransaction struct");

    snapshot_rpc_getrawtransaction(get_raw_transaction, &settings);

    // `getaddresstxids`
    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: 1,
            end: 10,
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids(get_address_tx_ids, &settings);

    // `getaddressutxos`
    let get_address_utxos = rpc
        .get_address_utxos(AddressStrings { addresses })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddressutxos(get_address_utxos, &settings);
}

/// Snapshot `getinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getinfo(info: GetInfo, settings: &insta::Settings) {
    settings.bind(|| {
        insta::assert_json_snapshot!("get_info", info, {
            ".subversion" => dynamic_redaction(|value, _path| {
                // assert that the subversion value is user agent
                assert_eq!(value.as_str().unwrap(), USER_AGENT);
                // replace with:
                "[SubVersion]"
            }),
        })
    });
}

/// Snapshot `getblockchaininfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockchaininfo(info: GetBlockChainInfo, settings: &insta::Settings) {
    settings.bind(|| {
        insta::assert_json_snapshot!("get_blockchain_info", info, {
            ".estimatedheight" => dynamic_redaction(|value, _path| {
                // assert that the value looks like a valid height here
                assert!(u32::try_from(value.as_u64().unwrap()).unwrap() < Height::MAX_AS_U32);
                // replace with:
                "[Height]"
            }),
        })
    });
}

/// Snapshot `getaddressbalance` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getaddressbalance(address_balance: AddressBalance, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_address_balance", address_balance));
}

/// Check `getblock` response, using `cargo insta`, JSON serialization, and block test vectors.
///
/// The snapshot file does not contain any data, but it does enforce the response format.
fn snapshot_rpc_getblock(block: GetBlock, block_data: &[u8], settings: &insta::Settings) {
    let block_data = hex::encode(block_data);

    settings.bind(|| {
        insta::assert_json_snapshot!("get_block", block, {
            "." => dynamic_redaction(move |value, _path| {
                // assert that the block data matches, without creating a 1.5 kB snapshot file
                assert_eq!(value.as_str().unwrap(), block_data);
                // replace with:
                "[BlockData]"
            }),
        })
    });
}

/// Check `getblock` response with verbosity=1, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblock_verbose(block: GetBlock, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_verbose", block));
}

/// Snapshot `getbestblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getbestblockhash(tip_hash: GetBestBlockHash, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_best_block_hash", tip_hash));
}

/// Snapshot `getrawmempool` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getrawmempool(raw_mempool: Vec<String>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_raw_mempool", raw_mempool));
}

/// Snapshot `z_gettreestate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_gettreestate(tree_state: GetTreestate, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("z_get_treestate", tree_state));
}

/// Snapshot `getrawtransaction` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getrawtransaction(raw_transaction: GetRawTransaction, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_raw_transaction", raw_transaction));
}

/// Snapshot `getaddressbalance` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getaddresstxids(transactions: Vec<String>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_address_tx_ids", transactions));
}

/// Snapshot `getaddressutxos` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getaddressutxos(utxos: Vec<GetAddressUtxos>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_address_utxos", utxos));
}

/// Utility function to convert a `Network` to a lowercase string.
fn network_string(network: Network) -> String {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();
    net_suffix
}
