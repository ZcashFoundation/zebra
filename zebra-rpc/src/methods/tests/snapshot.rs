//! RPC responses

use insta::dynamic_redaction;
use std::sync::Arc;

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
///
/// TODO:
/// - Add a `z_gettreestate` test when #3990 is merged.
#[tokio::test]
async fn test_rpc_response_data() {
    zebra_test::init();

    test_rpc_response_data_for_network(Mainnet).await;
    test_rpc_response_data_for_network(Testnet).await;
}

async fn test_rpc_response_data_for_network(network: Network) {
    // Create a continuous chain of mainnet and testnet blocks from genesis
    let blocks = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    let blocks: Vec<Arc<Block>> = blocks
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    // Create a populated state service
    let (_state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), Mainnet).await;

    // Init RPC
    let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        Mainnet,
    );

    // Start snapshots of RPC responses.

    // `getinfo`
    let get_info = rpc.get_info().expect("We should have a GetInfo struct");
    snapshot_rpc_getinfo(get_info, network);

    // `getblockchaininfo`
    let get_blockchain_info = rpc
        .get_blockchain_info()
        .expect("We should have a GetInfo struct");
    snapshot_rpc_getblockchaininfo(get_blockchain_info, network);

    // get the first transaction of the first block which is not the genesis
    let first_block_first_transaction = &blocks[1].transactions[0];

    // build addresses
    let address = &first_block_first_transaction.outputs()[1]
        .address(Mainnet)
        .unwrap();
    let addresses = vec![address.to_string()];

    // `getaddressbalance`
    let get_address_balance = rpc
        .get_address_balance(AddressStrings {
            addresses: addresses.clone(),
        })
        .await
        .expect("We should have an AddressBalance struct");
    snapshot_rpc_getaddressbalance(get_address_balance, network);

    // `getblock`
    let get_block = rpc
        .get_block("1".to_string(), 0u8)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock(get_block, network);

    // `getbestblockhash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBestBlockHash struct");
    snapshot_rpc_getbestblockhash(get_best_block_hash, network);

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

    snapshot_rpc_getrawmempool(get_raw_mempool, network);

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

    snapshot_rpc_getrawtransaction(get_raw_transaction, network);

    // `getaddresstxids`
    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: 1,
            end: 10,
        })
        .await
        .expect("We should have an vector of strings");
    snapshot_rpc_getaddresstxids(get_address_tx_ids, network);

    // `getaddressutxos`
    let get_address_utxos = rpc
        .get_address_utxos(AddressStrings { addresses })
        .await
        .expect("We should have an vector of strings");
    snapshot_rpc_getaddressutxos(get_address_utxos, network);
}

/// Snapshot `getinfo` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getinfo(info: GetInfo, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_info", network_string(network));
    settings.bind(|| {
        insta::assert_json_snapshot!(snap_name, info, {
            ".subversion" => dynamic_redaction(|value, _path| {
                // assert that the subversion value is user agent
                assert_eq!(value.as_str().unwrap(), USER_AGENT);
                // replace with:
                "[SubVersion]"
            }),
        })
    });
}

/// Snapshot `getblockchaininfo` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getblockchaininfo(info: GetBlockChainInfo, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_blockchain_info", network_string(network));
    settings.bind(|| {
        insta::assert_json_snapshot!(snap_name, info, {
            ".estimatedheight" => dynamic_redaction(|value, _path| {
                // assert that the value looks like a valid height here
                assert!(u32::try_from(value.as_u64().unwrap()).unwrap() < Height::MAX_AS_U32);
                // replace with:
                "[Height]"
            }),
        })
    });
}

/// Snapshot `getaddressbalance` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getaddressbalance(address_balance: AddressBalance, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_address_balance", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, address_balance));
}

/// Snapshot `getblock` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getblock(block: GetBlock, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_block", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, block));
}

/// Snapshot `getbestblockhash` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getbestblockhash(tip_hash: GetBestBlockHash, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_best_block_hash", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, tip_hash));
}

/// Snapshot `getrawmempool` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getrawmempool(raw_mempool: Vec<String>, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_raw_mempool", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, raw_mempool));
}

/// Snapshot `getrawtransaction` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getrawtransaction(raw_transaction: GetRawTransaction, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_raw_transaction", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, raw_transaction));
}

/// Snapshot `getaddressbalance` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getaddresstxids(transactions: Vec<String>, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_address_tx_ids", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, transactions));
}

/// Snapshot `getaddressutxos` response, using `cargo insta` and RON serialization.
fn snapshot_rpc_getaddressutxos(utxos: Vec<GetAddressUtxos>, network: Network) {
    let settings = insta::Settings::clone_current();
    let snap_name = format!("{}_get_address_utxos", network_string(network));
    settings.bind(|| insta::assert_json_snapshot!(snap_name, utxos));
}

/// Utility function to convert a `Network` to a lowercase string.
fn network_string(network: Network) -> String {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();
    net_suffix
}
