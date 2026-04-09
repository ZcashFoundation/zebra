//! Snapshot tests for Zebra JSON-RPC responses.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --release -p zebra-rpc --lib -- test_rpc_response_data
//! ```

use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use futures::FutureExt;
use hex::FromHex;
use insta::{dynamic_redaction, Settings};
use jsonrpsee::core::RpcResult as Result;
use tower::{buffer::Buffer, util::BoxService, Service};

use zcash_address::{ToAddress, ZcashAddress};
use zcash_protocol::consensus::NetworkType;
use zebra_chain::{
    block::{Block, Hash},
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::MockChainTip,
    orchard,
    parameters::{
        testnet::{self, ConfiguredActivationHeights, Parameters},
        Network::{self, Mainnet},
        NetworkKind, NetworkUpgrade,
    },
    serialization::{DateTime32, ZcashDeserializeInto},
    subtree::NoteCommitmentSubtreeData,
    transaction::Transaction,
    work::difficulty::CompactDifficulty,
};
use zebra_consensus::Request;
use zebra_network::{
    address_book_peers::MockAddressBookPeers,
    types::{MetaAddr, PeerServices},
};
use zebra_node_services::{mempool, BoxError};
use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse, MAX_ON_DISK_HEIGHT};
use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    vectors::BLOCK_MAINNET_1_BYTES,
};

use crate::methods::{
    hex_data::HexData,
    tests::utils::fake_history_tree,
    types::{
        get_block_template::GetBlockTemplateRequestMode,
        long_poll::{LongPollId, LONG_POLL_ID_LENGTH},
        peer_info::PeerInfo,
        subsidy::GetBlockSubsidyResponse,
    },
    GetBlockHashResponse,
};

use super::super::*;

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
        .expect("failed to set network name")
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(584_000),
            nu6: Some(2_976_000),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .to_network()
        .expect("failed to build configured network");

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

    let custom_testnet = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            sapling: Some(SAPLING_ACTIVATION_HEIGHT),
            // We need to set the NU5 activation height higher than the height of the last block for
            // this test because we currently have only the first 11 blocks from the public Testnet,
            // none of which are compatible with NU5 due to the following consensus rule:
            //
            // > [NU5 onward] hashBlockCommitments MUST be set to the value of
            // > hashBlockCommitments for this block, as specified in [ZIP-244].
            //
            // Activating NU5 at a lower height and using the 11 blocks causes a failure in
            // [`zebra_state::populated_state`].
            nu5: Some(11),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .clear_funding_streams()
        .with_network_name("custom_testnet")
        .expect("failed to set network name")
        .to_network()
        .expect("failed to build configured network");

    // Initiate the snapshots of the RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network_string(&custom_testnet).to_string());

    let blocks: Vec<_> = custom_testnet
        .blockchain_iter()
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (state, read_state, tip, _) =
        zebra_state::populated_state(blocks.clone(), &custom_testnet).await;
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _) = RpcImpl::new(
        custom_testnet,
        Default::default(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
        state,
        read_state,
        Buffer::new(MockService::build().for_unit_tests::<_, _, BoxError>(), 1),
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        rx,
        None,
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
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let mut mempool: MockService<_, _, _, zebra_node_services::BoxError> =
        MockService::build().for_unit_tests();

    // Create a populated state service
    let (state, read_state, tip, _) = zebra_state::populated_state(blocks.clone(), network).await;

    // Start snapshots of RPC responses.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(format!("{}_{}", network_string(network), blocks.len() - 1));

    let (block_verifier_router, _, _, _) = zebra_consensus::router::init_test(
        zebra_consensus::Config::default(),
        network,
        state.clone(),
    )
    .await;

    test_mining_rpcs(
        network,
        mempool.clone(),
        state.clone(),
        read_state.clone(),
        block_verifier_router.clone(),
        settings.clone(),
    )
    .await;

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        Default::default(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        state,
        read_state,
        block_verifier_router,
        MockSyncStatus::default(),
        tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    // We only want a snapshot of the `getblocksubsidy` and `getblockchaininfo` methods for the
    // non-default Testnet (with an NU6 activation height).
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
        .get_address_balance(GetAddressBalanceRequest {
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

    // `getmempoolinfo`
    //
    // - this RPC method returns mempool stats like size and bytes
    // - we simulate a call to the mempool with the `QueueStats` request,
    //   and respond with mock stats to verify RPC output formatting.
    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::QueueStats))
        .map(|responder| {
            responder.respond(mempool::Response::QueueStats {
                size: 67,
                bytes: 32_500,
                usage: 41_000,
                fully_notified: None,
            });
        });

    let (rsp, _) = futures::join!(rpc.get_mempool_info(), mempool_req);
    if let Ok(inner) = rsp {
        insta::assert_json_snapshot!("get_mempool_info", inner);
    } else {
        panic!("getmempoolinfo RPC must return a valid response");
    }

    // `getrawmempool`
    //
    // - a request to get all mempool transactions will be made by `getrawmempool` behind the scenes.
    // - as we have the mempool mocked we need to expect a request and wait for a response,
    // which will be an empty mempool in this case.
    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::FullTransactions))
        .map(|responder| {
            responder.respond(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: blocks[blocks.len() - 1].hash(),
            });
        });

    let (rsp, _) = futures::join!(rpc.get_raw_mempool(Some(true)), mempool_req);

    match rsp {
        Ok(GetRawMempoolResponse::Verbose(rsp)) => {
            settings.bind(|| insta::assert_json_snapshot!("get_raw_mempool_verbose", rsp));
        }
        _ => panic!("getrawmempool RPC must return `GetRawMempool::Verbose`"),
    }

    let mempool_req = mempool
        .expect_request_that(|request| matches!(request, mempool::Request::TransactionIds))
        .map(|responder| {
            responder.respond(mempool::Response::TransactionIds(Default::default()));
        });

    let (rsp, _) = futures::join!(rpc.get_raw_mempool(Some(false)), mempool_req);

    match rsp {
        Ok(GetRawMempoolResponse::TxIds(ref rsp)) => {
            settings.bind(|| insta::assert_json_snapshot!("get_raw_mempool", rsp));
        }
        _ => panic!("getrawmempool RPC must return `GetRawMempool::TxIds`"),
    }

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

    let rpc_req = rpc.get_raw_transaction(txid.clone(), Some(0u8), None);
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

    let rpc_req = rpc.get_raw_transaction(txid, Some(1u8), None);
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

    let rpc_req =
        rpc.get_raw_transaction(transaction::Hash::from([0; 32]).encode_hex(), Some(1), None);
    let (rsp, _) = futures::join!(rpc_req, mempool_req);
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_unknown_txid"), rsp));
    mempool.expect_no_requests().await;

    // `getrawtransaction` with an invalid TXID
    let rsp = rpc
        .get_raw_transaction("aBadC0de".to_owned(), Some(1), None)
        .await;
    settings.bind(|| insta::assert_json_snapshot!(format!("getrawtransaction_invalid_txid"), rsp));
    mempool.expect_no_requests().await;

    // `getaddresstxids`
    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: Some(1),
            end: Some(10),
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("multi_block", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: Some(2),
            end: Some(2),
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("single_block", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: Some(3),
            end: Some(EXCESSIVE_BLOCK_HEIGHT),
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("excessive_end", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: Some(EXCESSIVE_BLOCK_HEIGHT),
            end: Some(EXCESSIVE_BLOCK_HEIGHT + 1),
        })
        .await
        .expect("We should have a vector of strings");
    snapshot_rpc_getaddresstxids_valid("excessive_start", get_address_tx_ids, &settings);

    let get_address_tx_ids = rpc
        .get_address_tx_ids(GetAddressTxIdsRequest {
            addresses: addresses.clone(),
            start: Some(2),
            end: Some(1),
        })
        .await;
    snapshot_rpc_getaddresstxids_invalid("end_greater_start", get_address_tx_ids, &settings);

    // `getaddressutxos`
    let get_address_utxos = rpc
        .get_address_utxos(GetAddressUtxosRequest::new(addresses, false))
        .await
        .expect("We should have a vector of strings");
    let GetAddressUtxosResponse::Utxos(addresses) = get_address_utxos else {
        panic!("We should have a GetAddressUtxosResponse::ChainInfoFalse struct");
    };
    snapshot_rpc_getaddressutxos(addresses, &settings);
}

async fn test_mocked_rpc_response_data_for_network(network: &Network) {
    // Prepare the test harness.

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix(network_string(network));

    let (latest_chain_tip, _) = MockChainTip::new();
    let state = MockService::build().for_unit_tests();
    let mut read_state = MockService::build().for_unit_tests();

    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _) = RpcImpl::new(
        network.clone(),
        Default::default(),
        false,
        "0.0.1",
        "RPC test",
        MockService::build().for_unit_tests(),
        state.clone(),
        read_state.clone(),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    // Test the response format from `z_getsubtreesbyindex` for Sapling.

    // Mock the data for the response.
    let mut subtrees = BTreeMap::new();
    let subtree_root = sapling_crypto::Node::from_bytes([0; 32]).unwrap();

    for i in 0..2u16 {
        let subtree = NoteCommitmentSubtreeData::new(Height(i.into()), subtree_root);
        subtrees.insert(i.into(), subtree);
    }

    // Prepare the response.
    let rsp = read_state
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
    let rsp = read_state
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
fn snapshot_rpc_getinfo(info: GetInfoResponse, settings: &insta::Settings) {
    settings.bind(|| {
        insta::assert_json_snapshot!("get_info", info, {
            ".subversion" => dynamic_redaction(|value, _path| {
                // assert that the subversion value is user agent
                assert_eq!(value.as_str().unwrap(), format!("RPC test"));
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
    info: GetBlockchainInfoResponse,
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
fn snapshot_rpc_getaddressbalance(
    address_balance: GetAddressBalanceResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("get_address_balance", address_balance));
}

/// Check valid `getblock` data response with verbosity=0, using `cargo insta`, JSON serialization,
/// and block test vectors.
///
/// The snapshot file does not contain any data, but it does enforce the response format.
fn snapshot_rpc_getblock_data(
    variant: &'static str,
    block: GetBlockResponse,
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
    block: GetBlockResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!(format!("get_block_verbose_{variant}"), block));
}

/// Check valid `getblockheader` response using `cargo insta`.
fn snapshot_rpc_getblockheader(
    variant: &'static str,
    block: GetBlockHeaderResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!(format!("get_block_header_{variant}"), block));
}

/// Check invalid height `getblock` response using `cargo insta`.
fn snapshot_rpc_getblock_invalid(
    variant: &'static str,
    response: Result<GetBlockResponse>,
    settings: &insta::Settings,
) {
    settings
        .bind(|| insta::assert_json_snapshot!(format!("get_block_invalid_{variant}"), response));
}

/// Snapshot `getbestblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getbestblockhash(tip_hash: GetBlockHashResponse, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_best_block_hash", tip_hash));
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
fn snapshot_rpc_getaddressutxos(utxos: Vec<Utxo>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_address_utxos", utxos));
}

/// Snapshot `getblockcount` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockcount(block_count: u32, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_count", block_count));
}

/// Snapshot valid `getblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockhash_valid(block_hash: GetBlockHashResponse, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_hash_valid", block_hash));
}

/// Snapshot invalid `getblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockhash_invalid(
    variant: &'static str,
    block_hash: Result<GetBlockHashResponse>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_hash_invalid_{variant}"), block_hash)
    });
}

/// Snapshot `getblocktemplate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblocktemplate(
    variant: &'static str,
    block_template: GetBlockTemplateResponse,
    coinbase_tx: Option<Transaction>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_template_{variant}"), block_template)
    });

    if let Some(coinbase_tx) = coinbase_tx {
        settings.bind(|| {
            insta::assert_ron_snapshot!(
                format!("get_block_template_{variant}.coinbase_tx"),
                coinbase_tx
            )
        });
    };
}

/// Snapshot `submitblock` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_submit_block_invalid(
    submit_block_response: SubmitBlockResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!("snapshot_rpc_submit_block_invalid", submit_block_response)
    });
}

/// Snapshot `getmininginfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getmininginfo(get_mining_info: GetMiningInfoResponse, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_mining_info", get_mining_info));
}

/// Snapshot `getblocksubsidy` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblocksubsidy(
    variant: &'static str,
    get_block_subsidy: GetBlockSubsidyResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_subsidy_{variant}"), get_block_subsidy)
    });
}

/// Snapshot `getnetworkinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getnetworkinfo(
    get_network_info: GetNetworkInfoResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("get_network_info", get_network_info));
}

/// Snapshot `getpeerinfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getpeerinfo(get_peer_info: Vec<PeerInfo>, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_peer_info", get_peer_info));
}

/// Snapshot `getnetworksolps` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getnetworksolps(get_network_sol_ps: u64, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_network_sol_ps", get_network_sol_ps));
}

/// Snapshot `validateaddress` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_validateaddress(
    variant: &'static str,
    validate_address: ValidateAddressResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("validate_address_{variant}"), validate_address)
    });
}

/// Snapshot `z_validateaddress` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_validateaddress(
    variant: &'static str,
    z_validate_address: ZValidateAddressResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_validate_address_{variant}"), z_validate_address)
    });
}

/// Snapshot valid `getdifficulty` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getdifficulty_valid(
    variant: &'static str,
    difficulty: f64,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_difficulty_valid_{variant}"), difficulty)
    });
}

/// Snapshot `snapshot_rpc_z_listunifiedreceivers` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_listunifiedreceivers(
    variant: &'static str,
    response: ZListUnifiedReceiversResponse,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_list_unified_receivers_{variant}"), response)
    });
}

/// Utility function to convert a `Network` to a lowercase string.
fn network_string(network: &Network) -> String {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();
    net_suffix
}

pub async fn test_mining_rpcs<State, ReadState>(
    network: &Network,
    mempool: MockService<
        mempool::Request,
        mempool::Response,
        PanicAssertion,
        zebra_node_services::BoxError,
    >,
    state: State,
    read_state: ReadState,
    block_verifier_router: Buffer<BoxService<Request, Hash, RouterError>, Request>,
    settings: Settings,
) where
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::Request>>::Future: Send,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ReadState as Service<zebra_state::ReadRequest>>::Future: Send,
{
    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    #[allow(clippy::unnecessary_struct_initialization)]
    let mining_conf = crate::config::mining::Config {
        miner_address: Some(ZcashAddress::from_transparent_p2sh(
            NetworkType::from(NetworkKind::from(network)),
            [0x7e; 20],
        )),
        extra_coinbase_data: None,
        // TODO: Use default field values when optional features are enabled in tests #8183
        internal_miner: true,
    };

    // nu5 block height
    let fake_tip_height = NetworkUpgrade::Nu5.activation_height(network).unwrap();
    // nu5 block hash
    let fake_tip_hash =
        Hash::from_hex("0000000000d723156d9b65ffcf4984da7a19675ed7e2f06d9e5d5188af087bf8").unwrap();

    //  nu5 block time + 1
    let fake_min_time = DateTime32::from(1654008606);
    // nu5 block time + 12
    let fake_cur_time = DateTime32::from(1654008617);
    // nu5 block time + 123
    let fake_max_time = DateTime32::from(1654008728);

    // Use a valid fractional difficulty for snapshots
    let pow_limit = network.target_difficulty_limit();
    let fake_difficulty = pow_limit * 2 / 3;
    let fake_difficulty = CompactDifficulty::from(fake_difficulty);

    let (mock_tip, mock_tip_sender) = MockChainTip::new();
    mock_tip_sender.send_best_tip_height(fake_tip_height);
    mock_tip_sender.send_best_tip_hash(fake_tip_hash);
    mock_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));

    let mock_address_book = MockAddressBookPeers::new(vec![MetaAddr::new_connected(
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            network.default_port(),
        )
        .into(),
        &PeerServices::NODE_NETWORK,
        false,
    )
    .into_new_meta_addr(Instant::now(), DateTime32::now())]);

    let (_tx, rx) = tokio::sync::watch::channel(None);

    let (rpc, _) = RpcImpl::new(
        network.clone(),
        mining_conf.clone(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        state,
        read_state,
        block_verifier_router.clone(),
        mock_sync_status.clone(),
        mock_tip.clone(),
        mock_address_book,
        rx.clone(),
        None,
    );

    if network.is_a_test_network() && !network.is_default_testnet() {
        let fake_future_nu6_block_height =
            NetworkUpgrade::Nu6.activation_height(network).unwrap().0 + 100_000;
        let get_block_subsidy = rpc
            .get_block_subsidy(Some(fake_future_nu6_block_height))
            .await
            .expect("We should have a success response");
        snapshot_rpc_getblocksubsidy("future_nu6_height", get_block_subsidy, &settings);
        // We only want a snapshot of the `getblocksubsidy` method for the non-default Testnet (with an NU6 activation height).
        return;
    }

    // `getblockcount`
    let get_block_count = rpc.get_block_count().expect("We should have a number");
    snapshot_rpc_getblockcount(get_block_count, &settings);

    // `getblockhash`
    const BLOCK_HEIGHT10: i32 = 10;

    let get_block_hash = rpc
        .get_block_hash(BLOCK_HEIGHT10)
        .await
        .expect("We should have a GetBlockHash struct");
    snapshot_rpc_getblockhash_valid(get_block_hash, &settings);

    let get_block_hash = rpc
        .get_block_hash(
            EXCESSIVE_BLOCK_HEIGHT
                .try_into()
                .expect("constant fits in i32"),
        )
        .await;
    snapshot_rpc_getblockhash_invalid("excessive_height", get_block_hash, &settings);

    // `getmininginfo`
    let get_mining_info = rpc
        .get_mining_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getmininginfo(get_mining_info, &settings);

    // `getblocksubsidy`
    let fake_future_block_height = fake_tip_height.0 + 100_000;
    let get_block_subsidy = rpc
        .get_block_subsidy(Some(fake_future_block_height))
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("future_height", get_block_subsidy, &settings);

    let get_block_subsidy = rpc
        .get_block_subsidy(None)
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("tip_height", get_block_subsidy, &settings);

    let get_block_subsidy = rpc
        .get_block_subsidy(Some(EXCESSIVE_BLOCK_HEIGHT))
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("excessive_height", get_block_subsidy, &settings);

    // `getnetworkinfo`
    let get_network_info = rpc
        .get_network_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getnetworkinfo(get_network_info, &settings);

    // `getpeerinfo`
    let get_peer_info = rpc
        .get_peer_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getpeerinfo(get_peer_info, &settings);

    // `getnetworksolps` (and `getnetworkhashps`)
    //
    // TODO: add tests for excessive num_blocks and height (#6688)
    //       add the same tests for get_network_hash_ps
    let get_network_sol_ps = rpc
        .get_network_sol_ps(None, None)
        .await
        .expect("We should have a success response");
    snapshot_rpc_getnetworksolps(get_network_sol_ps, &settings);

    // `getblocktemplate` - the following snapshots use a mock read_state

    // get a new empty state
    let state = MockService::build().for_unit_tests();
    let read_state = MockService::build().for_unit_tests();

    let make_mock_read_state_request_handler = || {
        let mut read_state = read_state.clone();

        async move {
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
                    chain_history_root: fake_history_tree(network).hash(),
                }));
        }
    };

    let make_mock_mempool_request_handler = || {
        let mut mempool = mempool.clone();

        async move {
            mempool
                .expect_request(mempool::Request::FullTransactions)
                .await
                .respond(mempool::Response::FullTransactions {
                    transactions: vec![],
                    transaction_dependencies: Default::default(),
                    // tip hash needs to match chain info for long poll requests
                    last_seen_tip_hash: fake_tip_hash,
                });
        }
    };

    // send tip hash and time needed for getblocktemplate rpc
    mock_tip_sender.send_best_tip_hash(fake_tip_hash);

    let (rpc_mock_state, _) = RpcImpl::new(
        network.clone(),
        mining_conf.clone(),
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool.clone(), 1),
        state.clone(),
        read_state.clone(),
        block_verifier_router,
        mock_sync_status.clone(),
        mock_tip.clone(),
        MockAddressBookPeers::default(),
        rx.clone(),
        None,
    );

    // Basic variant (default mode and no extra features)

    // Fake the ChainInfo and FullTransaction responses
    let mock_read_state_request_handler = make_mock_read_state_request_handler();
    let mock_mempool_request_handler = make_mock_mempool_request_handler();

    let get_block_template_fut = rpc_mock_state.get_block_template(None);

    let (get_block_template, ..) = tokio::join!(
        get_block_template_fut,
        mock_mempool_request_handler,
        mock_read_state_request_handler,
    );

    let GetBlockTemplateResponse::TemplateMode(get_block_template) =
        get_block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let coinbase_tx: Transaction = get_block_template
        .coinbase_txn
        .data
        .as_ref()
        .zcash_deserialize_into()
        .expect("coinbase bytes are valid");

    snapshot_rpc_getblocktemplate(
        "basic",
        (*get_block_template).into(),
        Some(coinbase_tx),
        &settings,
    );

    // long polling feature with submit old field

    let long_poll_id: LongPollId = "0"
        .repeat(LONG_POLL_ID_LENGTH)
        .parse()
        .expect("unexpected invalid LongPollId");

    // Fake the ChainInfo and FullTransaction responses
    let mock_read_state_request_handler = make_mock_read_state_request_handler();
    let mock_mempool_request_handler = make_mock_mempool_request_handler();

    let get_block_template_fut = rpc_mock_state.get_block_template(
        GetBlockTemplateParameters {
            long_poll_id: long_poll_id.into(),
            ..Default::default()
        }
        .into(),
    );

    let (get_block_template, ..) = tokio::join!(
        get_block_template_fut,
        mock_mempool_request_handler,
        mock_read_state_request_handler,
    );

    let GetBlockTemplateResponse::TemplateMode(get_block_template) =
        get_block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!(
            "this getblocktemplate call without parameters should return the `TemplateMode` variant of the response"
        )
    };

    let coinbase_tx: Transaction = get_block_template
        .coinbase_txn
        .data
        .as_ref()
        .zcash_deserialize_into()
        .expect("coinbase bytes are valid");

    snapshot_rpc_getblocktemplate(
        "long_poll",
        (*get_block_template).into(),
        Some(coinbase_tx),
        &settings,
    );

    // `getblocktemplate` proposal mode variant

    let get_block_template = rpc_mock_state.get_block_template(Some(GetBlockTemplateParameters {
        mode: GetBlockTemplateRequestMode::Proposal,
        data: Some(HexData("".into())),
        ..Default::default()
    }));

    let get_block_template = get_block_template
        .await
        .expect("unexpected error in getblocktemplate RPC call");

    snapshot_rpc_getblocktemplate("invalid-proposal", get_block_template, None, &settings);

    // the following snapshots use a mock read_state and block_verifier_router

    let mut mock_block_verifier_router = MockService::build().for_unit_tests();
    let (rpc_mock_state_verifier, _) = RpcImpl::new(
        network.clone(),
        mining_conf,
        false,
        "0.0.1",
        "RPC test",
        Buffer::new(mempool, 1),
        state.clone(),
        read_state.clone(),
        mock_block_verifier_router.clone(),
        mock_sync_status,
        mock_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let get_block_template_fut =
        rpc_mock_state_verifier.get_block_template(Some(GetBlockTemplateParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            data: Some(HexData(BLOCK_MAINNET_1_BYTES.to_vec())),
            ..Default::default()
        }));

    let mock_block_verifier_router_request_handler = async move {
        mock_block_verifier_router
            .expect_request_that(|req| matches!(req, zebra_consensus::Request::CheckProposal(_)))
            .await
            .respond(Hash::from([0; 32]));
    };

    let (get_block_template, ..) = tokio::join!(
        get_block_template_fut,
        mock_block_verifier_router_request_handler,
    );

    let get_block_template =
        get_block_template.expect("unexpected error in getblocktemplate RPC call");

    snapshot_rpc_getblocktemplate("proposal", get_block_template, None, &settings);

    // These RPC snapshots use the populated state

    // `submitblock`

    let submit_block = rpc
        .submit_block(HexData("".into()), None)
        .await
        .expect("unexpected error in submitblock RPC call");

    snapshot_rpc_submit_block_invalid(submit_block, &settings);

    // `validateaddress`
    let founder_address = if network.is_mainnet() {
        "t3fqvkzrrNaMcamkQMwAyHRjfDdM2xQvDTR"
    } else {
        "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi"
    };

    let validate_address = rpc
        .validate_address(founder_address.to_string())
        .await
        .expect("We should have a validate_address::Response");
    snapshot_rpc_validateaddress("basic", validate_address, &settings);

    let validate_address = rpc
        .validate_address("".to_string())
        .await
        .expect("We should have a validate_address::Response");
    snapshot_rpc_validateaddress("invalid", validate_address, &settings);

    // `z_validateaddress`
    let founder_address = if network.is_mainnet() {
        "t3fqvkzrrNaMcamkQMwAyHRjfDdM2xQvDTR"
    } else {
        "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi"
    };

    let z_validate_address = rpc
        .z_validate_address(founder_address.to_string())
        .await
        .expect("We should have a z_validate_address::Response");
    snapshot_rpc_z_validateaddress("basic", z_validate_address, &settings);

    let z_validate_address = rpc
        .z_validate_address("".to_string())
        .await
        .expect("We should have a z_validate_address::Response");
    snapshot_rpc_z_validateaddress("invalid", z_validate_address, &settings);

    // `getdifficulty`
    // This RPC snapshot uses both the mock and populated states

    // Fake the ChainInfo response using the mock state
    let mock_read_state_request_handler = make_mock_read_state_request_handler();

    let get_difficulty_fut = rpc_mock_state.get_difficulty();

    let (get_difficulty, ..) = tokio::join!(get_difficulty_fut, mock_read_state_request_handler,);

    let mock_get_difficulty = get_difficulty.expect("unexpected error in getdifficulty RPC call");

    snapshot_rpc_getdifficulty_valid("mock", mock_get_difficulty, &settings);

    // `z_listunifiedreceivers`

    let ua1 = String::from(
        "u1l8xunezsvhq8fgzfl7404m450nwnd76zshscn6nfys7vyz2ywyh4cc5daaq0c7q2su5lqfh23sp7fkf3kt27ve5948mzpfdvckzaect2jtte308mkwlycj2u0eac077wu70vqcetkxf",
    );
    let z_list_unified_receivers = rpc
        .z_list_unified_receivers(ua1)
        .await
        .expect("unexpected error in z_list_unified_receivers RPC call");

    snapshot_rpc_z_listunifiedreceivers("ua1", z_list_unified_receivers, &settings);

    let ua2 = String::from(
        "u1uf4qsmh037x2jp6k042h9d2w22wfp39y9cqdf8kcg0gqnkma2gf4g80nucnfeyde8ev7a6kf0029gnwqsgadvaye9740gzzpmr67nfkjjvzef7rkwqunqga4u4jges4tgptcju5ysd0",
    );
    let z_list_unified_receivers = rpc
        .z_list_unified_receivers(ua2)
        .await
        .expect("unexpected error in z_list_unified_receivers RPC call");

    snapshot_rpc_z_listunifiedreceivers("ua2", z_list_unified_receivers, &settings);
}
