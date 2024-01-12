//! Snapshot tests for Zebra JSON-RPC responses.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review
//! ```

use std::{collections::BTreeMap, sync::Arc};

use insta::dynamic_redaction;
use tower::buffer::Buffer;

use zebra_chain::{
    block::Block,
    chain_tip::mock::MockChainTip,
    parameters::Network::{Mainnet, Testnet},
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtreeData,
};
use zebra_state::{ReadRequest, ReadResponse, MAX_ON_DISK_HEIGHT};
use zebra_test::mock_service::MockService;

use super::super::*;

#[cfg(feature = "getblocktemplate-rpcs")]
mod get_block_template_rpcs;

/// The first block height in the state that can never be stored in the database,
/// due to optimisations in the disk format.
pub const EXCESSIVE_BLOCK_HEIGHT: u32 = MAX_ON_DISK_HEIGHT.0 + 1;

/// Snapshot test for RPC methods responses.
#[tokio::test(flavor = "multi_thread")]
async fn test_rpc_response_data() {
    let _init_guard = zebra_test::init();

    tokio::join!(
        test_rpc_response_data_for_network(Mainnet),
        test_rpc_response_data_for_network(Testnet),
        test_mocked_rpc_response_data_for_network(Mainnet),
        test_mocked_rpc_response_data_for_network(Testnet),
    );
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

    let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> =
        MockService::build().for_unit_tests();
    // Create a populated state service
    #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), network).await;

    // Start snapshots of RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_{}", network_string(network), blocks.len() - 1));

    // Test getblocktemplate-rpcs snapshots
    #[cfg(feature = "getblocktemplate-rpcs")]
    get_block_template_rpcs::test_responses(
        network,
        mempool.clone(),
        state,
        read_state.clone(),
        settings.clone(),
    )
    .await;

    // Init RPC
    let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
        "RPC test",
        "/Zebra:RPC test/",
        network,
        false,
        true,
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
    );

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

    // `getblock` variants
    // A valid block height in the populated state
    const BLOCK_HEIGHT: u32 = 1;

    let block_hash = blocks[BLOCK_HEIGHT as usize].hash();

    // `getblock`, verbosity=0, height
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), Some(0u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_data(
        "height_verbosity_0",
        get_block,
        block_data.get(&BLOCK_HEIGHT).unwrap(),
        &settings,
    );

    let get_block = rpc
        .get_block(EXCESSIVE_BLOCK_HEIGHT.to_string(), Some(0u8))
        .await;
    snapshot_rpc_getblock_invalid("excessive_height_verbosity_0", get_block, &settings);

    // `getblock`, verbosity=0, hash
    let get_block = rpc
        .get_block(block_hash.to_string(), Some(0u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_data(
        "hash_verbosity_0",
        get_block,
        block_data.get(&BLOCK_HEIGHT).unwrap(),
        &settings,
    );

    // `getblock`, verbosity=1, height
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), Some(1u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("height_verbosity_1", get_block, &settings);

    let get_block = rpc
        .get_block(EXCESSIVE_BLOCK_HEIGHT.to_string(), Some(1u8))
        .await;
    snapshot_rpc_getblock_invalid("excessive_height_verbosity_1", get_block, &settings);

    // `getblock`, verbosity=1, hash
    let get_block = rpc
        .get_block(block_hash.to_string(), Some(1u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("hash_verbosity_1", get_block, &settings);

    // `getblock`, no verbosity - defaults to 1, height
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), None)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("height_verbosity_default", get_block, &settings);

    let get_block = rpc
        .get_block(EXCESSIVE_BLOCK_HEIGHT.to_string(), None)
        .await;
    snapshot_rpc_getblock_invalid("excessive_height_verbosity_default", get_block, &settings);

    // `getblock`, no verbosity - defaults to 1, hash
    let get_block = rpc
        .get_block(block_hash.to_string(), None)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("hash_verbosity_default", get_block, &settings);

    // `getbestblockhash`
    let get_best_block_hash = rpc
        .get_best_block_hash()
        .expect("We should have a GetBlockHash struct");
    snapshot_rpc_getbestblockhash(get_best_block_hash, &settings);

    // `getrawmempool`
    //
    // - a request to get all mempool transactions will be made by `getrawmempool` behind the scenes.
    // - as we have the mempool mocked we need to expect a request and wait for a response,
    // which will be an empty mempool in this case.
    // Note: this depends on `SHOULD_USE_ZCASHD_ORDER` being true.
    #[cfg(feature = "getblocktemplate-rpcs")]
    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .map(|responder| {
            responder.respond(mempool::Response::FullTransactions {
                transactions: vec![],
                last_seen_tip_hash: blocks[blocks.len() - 1].hash(),
            });
        });

    #[cfg(not(feature = "getblocktemplate-rpcs"))]
    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::TransactionIds))
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
    snapshot_rpc_z_gettreestate_valid(tree_state, &settings);

    let tree_state = rpc
        .z_get_treestate(EXCESSIVE_BLOCK_HEIGHT.to_string())
        .await;
    snapshot_rpc_z_gettreestate_invalid("excessive_height", tree_state, &settings);

    // `getrawtransaction` verbosity=0
    //
    // - similar to `getrawmempool` described above, a mempool request will be made to get the requested
    // transaction from the mempool, response will be empty as we have this transaction in state
    let mempool_req = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::TransactionsByMinedId(_))
        })
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    // make the api call
    let get_raw_transaction =
        rpc.get_raw_transaction(first_block_first_transaction.hash().encode_hex(), Some(0u8));
    let (response, _) = futures::join!(get_raw_transaction, mempool_req);
    let get_raw_transaction = response.expect("We should have a GetRawTransaction struct");

    snapshot_rpc_getrawtransaction("verbosity_0", get_raw_transaction, &settings);

    // `getrawtransaction` verbosity=1
    let mempool_req = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::TransactionsByMinedId(_))
        })
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    // make the api call
    let get_raw_transaction =
        rpc.get_raw_transaction(first_block_first_transaction.hash().encode_hex(), Some(1u8));
    let (response, _) = futures::join!(get_raw_transaction, mempool_req);
    let get_raw_transaction = response.expect("We should have a GetRawTransaction struct");

    snapshot_rpc_getrawtransaction("verbosity_1", get_raw_transaction, &settings);

    // `getaddresstxids`
    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: 1,
            end: 10,
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("multi_block", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: 2,
            end: 2,
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("single_block", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: 3,
            end: EXCESSIVE_BLOCK_HEIGHT,
        })
        .await;
    snapshot_rpc_getaddresstxids_invalid("excessive_end", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: EXCESSIVE_BLOCK_HEIGHT,
            end: EXCESSIVE_BLOCK_HEIGHT + 1,
        })
        .await;
    snapshot_rpc_getaddresstxids_invalid("excessive_start", get_address_tx_ids, &settings);

    // `getaddressutxos`
    let get_address_utxos = rpc
        .get_address_utxos(AddressStrings { addresses })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddressutxos(get_address_utxos, &settings);
}

async fn test_mocked_rpc_response_data_for_network(network: Network) {
    // Prepare the test harness.

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network_string(network));

    let (latest_chain_tip, _) = MockChainTip::new();
    let mut state = MockService::build().for_unit_tests();
    let mempool = MockService::build().for_unit_tests();

    let (rpc, _) = RpcImpl::new(
        "RPC test",
        "/Zebra:RPC test/",
        network,
        false,
        true,
        mempool,
        state.clone(),
        latest_chain_tip,
    );

    // Test the response format from `z_getsubtreesbyindex` for Sapling.

    // Mock the data for the response.
    let mut subtrees = BTreeMap::new();
    let subtree_root = sapling::tree::Node::default();

    for i in 0..2u16 {
        let subtree = NoteCommitmentSubtreeData::new(Height(i.into()), subtree_root);
        subtrees.insert(i.into(), subtree);
    }

    // Prepare the response.
    let rsp = state
        .expect_request_that(|req| matches!(req, ReadRequest::SaplingSubtrees { .. }))
        .map(|responder| responder.respond(ReadResponse::SaplingSubtrees(subtrees)));

    // Make the request.
    let req = rpc.z_get_subtrees_by_index(String::from("sapling"), 0u16.into(), Some(2u16.into()));

    // Get the response.
    let (subtrees_rsp, ..) = tokio::join!(req, rsp);
    let subtrees = subtrees_rsp.expect("The RPC response should contain a `GetSubtrees` struct.");

    // Check the response.
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_get_subtrees_by_index_for_sapling"), subtrees)
    });

    // Test the response format from `z_getsubtreesbyindex` for Orchard.

    // Mock the data for the response.
    let mut subtrees = BTreeMap::new();
    let subtree_root = orchard::tree::Node::default();

    for i in 0..2u16 {
        let subtree = NoteCommitmentSubtreeData::new(Height(i.into()), subtree_root);
        subtrees.insert(i.into(), subtree);
    }

    // Prepare the response.
    let rsp = state
        .expect_request_that(|req| matches!(req, ReadRequest::OrchardSubtrees { .. }))
        .map(|responder| responder.respond(ReadResponse::OrchardSubtrees(subtrees)));

    // Make the request.
    let req = rpc.z_get_subtrees_by_index(String::from("orchard"), 0u16.into(), Some(2u16.into()));

    // Get the response.
    let (subtrees_rsp, ..) = tokio::join!(req, rsp);
    let subtrees = subtrees_rsp.expect("The RPC response should contain a `GetSubtrees` struct.");

    // Check the response.
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_get_subtrees_by_index_for_orchard"), subtrees)
    });
}

/// Snapshot `getinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getinfo(info: GetInfo, settings: &insta::Settings) {
    settings.bind(|| {
        insta::assert_json_snapshot!("get_info", info, {
            ".subversion" => dynamic_redaction(|value, _path| {
                // assert that the subversion value is user agent
                assert_eq!(value.as_str().unwrap(), format!("/Zebra:RPC test/"));
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

/// Check valid `getblock` data response with verbosity=0, using `cargo insta`, JSON serialization,
/// and block test vectors.
///
/// The snapshot file does not contain any data, but it does enforce the response format.
fn snapshot_rpc_getblock_data(
    variant: &'static str,
    block: GetBlock,
    expected_block_data: &[u8],
    settings: &insta::Settings,
) {
    let expected_block_data = hex::encode(expected_block_data);

    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_data_{variant}"), block, {
            "." => dynamic_redaction(move |value, _path| {
                // assert that the block data matches, without creating a 1.5 kB snapshot file
                assert_eq!(value.as_str().unwrap(), expected_block_data);
                // replace with:
                "[BlockData]"
            }),
        })
    });
}

/// Check valid `getblock` response with verbosity=1, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblock_verbose(
    variant: &'static str,
    block: GetBlock,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!(format!("get_block_verbose_{variant}"), block));
}

/// Check invalid height `getblock` response using `cargo insta`.
fn snapshot_rpc_getblock_invalid(
    variant: &'static str,
    response: Result<GetBlock>,
    settings: &insta::Settings,
) {
    settings
        .bind(|| insta::assert_json_snapshot!(format!("get_block_invalid_{variant}"), response));
}

/// Snapshot `getbestblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getbestblockhash(tip_hash: GetBlockHash, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_best_block_hash", tip_hash));
}

/// Snapshot `getrawmempool` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getrawmempool(raw_mempool: Vec<String>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_raw_mempool", raw_mempool));
}

/// Snapshot a valid `z_gettreestate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_gettreestate_valid(tree_state: GetTreestate, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!(format!("z_get_treestate_valid"), tree_state));
}

/// Snapshot an invalid `z_gettreestate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_gettreestate_invalid(
    variant: &'static str,
    tree_state: Result<GetTreestate>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_get_treestate_invalid_{variant}"), tree_state)
    });
}

/// Snapshot `getrawtransaction` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getrawtransaction(
    variant: &'static str,
    raw_transaction: GetRawTransaction,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_raw_transaction_{variant}"), raw_transaction)
    });
}

/// Snapshot valid `getaddressbalance` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getaddresstxids_valid(
    variant: &'static str,
    transactions: Vec<String>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_address_tx_ids_valid_{variant}"), transactions)
    });
}

/// Snapshot invalid `getaddressbalance` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getaddresstxids_invalid(
    variant: &'static str,
    transactions: Result<Vec<String>>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(
            format!("get_address_tx_ids_invalid_{variant}"),
            transactions
        )
    });
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
