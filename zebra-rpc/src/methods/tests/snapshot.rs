//! Snapshot tests for Zebra JSON-RPC responses.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --release -p zebra-rpc --lib -- test_rpc_response_data
//! ```

use std::{collections::BTreeMap, sync::Arc};

use futures::FutureExt;
use insta::dynamic_redaction;
use jsonrpsee::core::RpcResult as Result;
use tower::buffer::Buffer;

use zebra_chain::{
    block::Block,
    chain_tip::mock::MockChainTip,
    orchard,
    parameters::{
        subsidy::POST_NU6_FUNDING_STREAMS_TESTNET,
        testnet::{self, ConfiguredActivationHeights, Parameters},
        Network::Mainnet,
    },
    sapling,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtreeData,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_state::{ReadRequest, ReadResponse, MAX_ON_DISK_HEIGHT};
use zebra_test::mock_service::MockService;

use super::super::*;

mod get_block_template_rpcs;

/// The first block height in the state that can never be stored in the database,
/// due to optimisations in the disk format.
pub const EXCESSIVE_BLOCK_HEIGHT: u32 = MAX_ON_DISK_HEIGHT.0 + 1;

/// Snapshot test for RPC methods responses.
#[tokio::test(flavor = "multi_thread")]
async fn test_rpc_response_data() {
    let _init_guard = zebra_test::init();
    let default_testnet = Network::new_default_testnet();
    let nu6_testnet = testnet::Parameters::build()
        .with_network_name("NU6Testnet")
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(584_000),
            nu6: Some(POST_NU6_FUNDING_STREAMS_TESTNET.height_range().start.0),
            ..Default::default()
        })
        .to_network();

    tokio::join!(
        test_rpc_response_data_for_network(&Mainnet),
        test_rpc_response_data_for_network(&default_testnet),
        test_rpc_response_data_for_network(&nu6_testnet),
        test_mocked_rpc_response_data_for_network(&Mainnet),
        test_mocked_rpc_response_data_for_network(&default_testnet),
    );
}

/// Checks the output of the [`z_get_treestate`] RPC.
///
/// TODO:
/// 1. Check a non-empty Sapling treestate.
/// 2. Check an empty Orchard treestate at NU5 activation height.
/// 3. Check a non-empty Orchard treestate.
///
/// To implement the todos above, we need to:
///
/// 1. Have a block containing Sapling note commitmnets in the state.
/// 2. Activate NU5 at a height for which we have a block in the state.
/// 3. Have a block containing Orchard note commitments in the state.
#[tokio::test]
async fn test_z_get_treestate() {
    let _init_guard = zebra_test::init();
    const SAPLING_ACTIVATION_HEIGHT: u32 = 2;

    let testnet = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            sapling: Some(SAPLING_ACTIVATION_HEIGHT),
            // We need to set the NU5 activation height higher than the height of the last block for
            // this test because we currently have only the first 10 blocks from the public Testnet,
            // none of which are compatible with NU5 due to the following consensus rule:
            //
            // > [NU5 onward] hashBlockCommitments MUST be set to the value of
            // > hashBlockCommitments for this block, as specified in [ZIP-244].
            //
            // Activating NU5 at a lower height and using the 10 blocks causes a failure in
            // [`zebra_state::populated_state`].
            nu5: Some(10),
            ..Default::default()
        })
        .with_network_name("custom_testnet")
        .to_network();

    // Initiate the snapshots of the RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network_string(&testnet).to_string());

    let blocks: Vec<_> = testnet
        .blockchain_iter()
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_, state, tip, _) = zebra_state::populated_state(blocks.clone(), &testnet).await;

    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _) = RpcImpl::new(
        "",
        "",
        testnet,
        false,
        true,
        Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
        state,
        tip,
        MockAddressBookPeers::new(vec![]),
        rx,
    );

    // Request the treestate by a hash.
    let treestate = rpc
        .z_get_treestate(blocks[0].hash().to_string())
        .await
        .expect("genesis treestate = no treestate");
    settings.bind(|| insta::assert_json_snapshot!("z_get_treestate_by_hash", treestate));

    // Request the treestate by a hash for a block which is not in the state.
    let treestate = rpc.z_get_treestate(block::Hash([0; 32]).to_string()).await;
    settings
        .bind(|| insta::assert_json_snapshot!("z_get_treestate_by_non_existent_hash", treestate));

    // Request the treestate before Sapling activation.
    let treestate = rpc
        .z_get_treestate((SAPLING_ACTIVATION_HEIGHT - 1).to_string())
        .await
        .expect("no Sapling treestate and no Orchard treestate");
    settings.bind(|| insta::assert_json_snapshot!("z_get_treestate_no_treestate", treestate));

    // Request the treestate at Sapling activation.
    let treestate = rpc
        .z_get_treestate(SAPLING_ACTIVATION_HEIGHT.to_string())
        .await
        .expect("empty Sapling treestate and no Orchard treestate");
    settings.bind(|| {
        insta::assert_json_snapshot!("z_get_treestate_empty_Sapling_treestate", treestate)
    });

    // Request the treestate for an invalid height.
    let treestate = rpc
        .z_get_treestate(EXCESSIVE_BLOCK_HEIGHT.to_string())
        .await;
    settings
        .bind(|| insta::assert_json_snapshot!("z_get_treestate_excessive_block_height", treestate));

    // Request the treestate for an unparsable hash or height.
    let treestate = rpc.z_get_treestate("Do you even shield?".to_string()).await;
    settings.bind(|| {
        insta::assert_json_snapshot!("z_get_treestate_unparsable_hash_or_height", treestate)
    });

    // TODO:
    // 1. Request a non-empty Sapling treestate.
    // 2. Request an empty Orchard treestate at an NU5 activation height.
    // 3. Request a non-empty Orchard treestate.
}

async fn test_rpc_response_data_for_network(network: &Network) {
    // Create a continuous chain of mainnet and testnet blocks from genesis
    let block_data = network.blockchain_map();

    let blocks: Vec<Arc<Block>> = block_data
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> =
        MockService::build().for_unit_tests();

    // Create a populated state service
    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), network).await;

    // Start snapshots of RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_{}", network_string(network), blocks.len() - 1));

    // Test the `getblocktemplate` RPC snapshots.
    get_block_template_rpcs::test_responses(
        network,
        mempool.clone(),
        state,
        read_state.clone(),
        settings.clone(),
    )
    .await;

    // Init RPC
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
        "0.0.1",
        "/Zebra:RPC test/",
        network.clone(),
        false,
        true,
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        MockAddressBookPeers::new(vec![]),
        rx,
    );

    // We only want a snapshot of the `getblocksubsidy` and `getblockchaininfo` methods for the non-default Testnet (with an NU6 activation height).
    if network.is_a_test_network() && !network.is_default_testnet() {
        let get_blockchain_info = rpc
            .get_blockchain_info()
            .await
            .expect("We should have a GetBlockChainInfo struct");
        snapshot_rpc_getblockchaininfo("_future_nu6_height", get_blockchain_info, &settings);

        return;
    }

    // `getinfo`
    let get_info = rpc
        .get_info()
        .await
        .expect("We should have a GetInfo struct");
    snapshot_rpc_getinfo(get_info, &settings);

    // `getblockchaininfo`
    let get_blockchain_info = rpc
        .get_blockchain_info()
        .await
        .expect("We should have a GetBlockChainInfo struct");
    snapshot_rpc_getblockchaininfo("", get_blockchain_info, &settings);

    // get the first transaction of the first block which is not the genesis
    let first_block_first_tx = &blocks[1].transactions[0];

    // build addresses
    let address = &first_block_first_tx.outputs()[1].address(network).unwrap();
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

    // `getblock`, verbosity=2, height
    let get_block = rpc
        .get_block(BLOCK_HEIGHT.to_string(), Some(2u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("height_verbosity_2", get_block, &settings);

    let get_block = rpc
        .get_block(EXCESSIVE_BLOCK_HEIGHT.to_string(), Some(2u8))
        .await;
    snapshot_rpc_getblock_invalid("excessive_height_verbosity_2", get_block, &settings);

    // `getblock`, verbosity=2, hash
    let get_block = rpc
        .get_block(block_hash.to_string(), Some(2u8))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblock_verbose("hash_verbosity_2", get_block, &settings);

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

    // `getblockheader(hash, verbose = false)`
    let get_block_header = rpc
        .get_block_header(block_hash.to_string(), Some(false))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblockheader("hash", get_block_header, &settings);

    // `getblockheader(height, verbose = false)`
    let get_block_header = rpc
        .get_block_header(BLOCK_HEIGHT.to_string(), Some(false))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblockheader("height", get_block_header, &settings);

    // `getblockheader(hash, verbose = true)`
    let get_block_header = rpc
        .get_block_header(block_hash.to_string(), Some(true))
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblockheader("hash_verbose", get_block_header, &settings);

    // `getblockheader(height, verbose = true)` where verbose is the default value.
    let get_block_header = rpc
        .get_block_header(BLOCK_HEIGHT.to_string(), None)
        .await
        .expect("We should have a GetBlock struct");
    snapshot_rpc_getblockheader("height_verbose", get_block_header, &settings);

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
    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .map(|responder| {
            responder.respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: blocks[blocks.len() - 1].hash(),
            });
        });

    // make the api call
    let get_raw_mempool = rpc.get_raw_mempool(None);
    let (response, _) = futures::join!(get_raw_mempool, mempool_req);
    let GetRawMempool::TxIds(get_raw_mempool) =
        response.expect("We should have a GetRawTransaction struct")
    else {
        panic!("should return TxIds for non verbose");
    };

    snapshot_rpc_getrawmempool(get_raw_mempool, &settings);

    // `getrawtransaction` verbosity=0
    //
    // - Similarly to `getrawmempool` described above, a mempool request will be made to get the
    //   requested transaction from the mempool. Response will be empty as we have this transaction
    //   in the state.
    let mempool_req = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::TransactionsByMinedId(_))
        })
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    let txid = first_block_first_tx.hash().encode_hex::<String>();

    let rpc_req = rpc.get_raw_transaction(txid.clone(), Some(0u8));
    let (rsp, _) = futures::join!(rpc_req, mempool_req);
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_verbosity=0"), rsp));
    mempool.expect_no_requests().await;

    // `getrawtransaction` verbosity=1
    let mempool_req = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::TransactionsByMinedId(_))
        })
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    let rpc_req = rpc.get_raw_transaction(txid, Some(1u8));
    let (rsp, _) = futures::join!(rpc_req, mempool_req);
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_verbosity=1"), rsp));
    mempool.expect_no_requests().await;

    // `getrawtransaction` with unknown txid
    let mempool_req = mempool
        .expect_request_that(|request| {
            matches!(request, mempool::Request::TransactionsByMinedId(_))
        })
        .map(|responder| {
            responder.respond(mempool::Response::Transactions(vec![]));
        });

    let rpc_req = rpc.get_raw_transaction(transaction::Hash::from([0; 32]).encode_hex(), Some(1));
    let (rsp, _) = futures::join!(rpc_req, mempool_req);
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_unknown_txid"), rsp));
    mempool.expect_no_requests().await;

    // `getrawtransaction` with an invalid TXID
    let rsp = rpc
        .get_raw_transaction("aBadC0de".to_owned(), Some(1))
        .await;
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_invalid_txid"), rsp));
    mempool.expect_no_requests().await;

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

async fn test_mocked_rpc_response_data_for_network(network: &Network) {
    // Prepare the test harness.

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network_string(network));

    let (latest_chain_tip, _) = MockChainTip::new();
    let mut state = MockService::build().for_unit_tests();
    let mempool = MockService::build().for_unit_tests();

    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _) = RpcImpl::new(
        "0.0.1",
        "/Zebra:RPC test/",
        network.clone(),
        false,
        true,
        mempool,
        state.clone(),
        latest_chain_tip,
        MockAddressBookPeers::new(vec![]),
        rx,
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
            ".errorstimestamp" => dynamic_redaction(|_value, _path| {
                "[LastErrorTimestamp]"
            }),
        })
    });
}

/// Snapshot `getblockchaininfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockchaininfo(
    variant_suffix: &str,
    info: GetBlockChainInfo,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_blockchain_info{variant_suffix}"), info, {
            ".estimatedheight" => dynamic_redaction(|value, _path| {
                // assert that the value looks like a valid height here
                assert!(u32::try_from(value.as_u64().unwrap()).unwrap() < Height::MAX_AS_U32);
                // replace with:
                "[Height]"
            }),
            ".verificationprogress" => dynamic_redaction(|value, _path| {
                // assert that the value looks like a valid verification progress here
                assert!(value.as_f64().unwrap() <= 1.0);
                // replace with:
                "[f64]"
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

/// Check valid `getblockheader` response using `cargo insta`.
fn snapshot_rpc_getblockheader(
    variant: &'static str,
    block: GetBlockHeader,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!(format!("get_block_header_{variant}"), block));
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
fn network_string(network: &Network) -> String {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();
    net_suffix
}
