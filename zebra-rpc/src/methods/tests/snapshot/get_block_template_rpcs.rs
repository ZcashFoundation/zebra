//! Snapshot tests for getblocktemplate RPCs.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --features getblocktemplate-rpcs --delete-unreferenced-snapshots
//! ```

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Instant,
};

use hex::FromHex;
use insta::Settings;
use jsonrpc_core::Result;
use tower::{buffer::Buffer, Service};

use zebra_chain::{
    block::Hash,
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::MockChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, ZcashDeserializeInto},
    transaction::Transaction,
    transparent,
    work::difficulty::{CompactDifficulty, ParameterDifficulty as _},
};
use zebra_network::{address_book_peers::MockAddressBookPeers, types::MetaAddr};
use zebra_node_services::mempool;

use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};

use zebra_test::{
    mock_service::{MockService, PanicAssertion},
    vectors::BLOCK_MAINNET_1_BYTES,
};

use crate::methods::{
    get_block_template_rpcs::types::{
        get_block_template::{self, GetBlockTemplateRequestMode},
        get_mining_info,
        long_poll::{LongPollId, LONG_POLL_ID_LENGTH},
        peer_info::PeerInfo,
        submit_block,
        subsidy::BlockSubsidy,
        unified_address, validate_address, z_validate_address,
    },
    hex_data::HexData,
    tests::{snapshot::EXCESSIVE_BLOCK_HEIGHT, utils::fake_history_tree},
    GetBlockHash, GetBlockTemplateRpc, GetBlockTemplateRpcImpl,
};

pub async fn test_responses<State, ReadState>(
    network: &Network,
    mempool: MockService<
        mempool::Request,
        mempool::Response,
        PanicAssertion,
        zebra_node_services::BoxError,
    >,
    state: State,
    read_state: ReadState,
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
    let (
        block_verifier_router,
        _transaction_verifier,
        _parameter_download_task_handle,
        _max_checkpoint_height,
    ) = zebra_consensus::router::init(zebra_consensus::Config::default(), network, state.clone())
        .await;

    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    #[allow(clippy::unnecessary_struct_initialization)]
    let mining_config = crate::config::mining::Config {
        miner_address: Some(transparent::Address::from_script_hash(
            network.kind(),
            [0xad; 20],
        )),
        extra_coinbase_data: None,
        debug_like_zcashd: true,
        // TODO: Use default field values when optional features are enabled in tests #8183
        ..Default::default()
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

    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();
    mock_chain_tip_sender.send_best_tip_height(fake_tip_height);
    mock_chain_tip_sender.send_best_tip_hash(fake_tip_hash);
    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(Some(0));

    let mock_address_book = MockAddressBookPeers::new(vec![MetaAddr::new_initial_peer(
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            network.default_port(),
        )
        .into(),
    )
    .into_new_meta_addr(Instant::now(), DateTime32::now())]);

    // get an rpc instance with continuous blockchain state
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        network,
        mining_config.clone(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        mock_chain_tip.clone(),
        block_verifier_router.clone(),
        mock_sync_status.clone(),
        mock_address_book,
    );

    if network.is_a_test_network() && !network.is_default_testnet() {
        // FIXME: Would this work after Nu7 activation?
        let fake_future_nu6_block_height =
            NetworkUpgrade::Nu6.activation_height(network).unwrap().0 + 100_000;
        let get_block_subsidy = get_block_template_rpc
            .get_block_subsidy(Some(fake_future_nu6_block_height))
            .await
            .expect("We should have a success response");
        snapshot_rpc_getblocksubsidy("future_nu6_height", get_block_subsidy, &settings);
        // We only want a snapshot of the `getblocksubsidy` method for the non-default Testnet (with an NU6 activation height).
        return;
    }

    // `getblockcount`
    let get_block_count = get_block_template_rpc
        .get_block_count()
        .expect("We should have a number");
    snapshot_rpc_getblockcount(get_block_count, &settings);

    // `getblockhash`
    const BLOCK_HEIGHT10: i32 = 10;

    let get_block_hash = get_block_template_rpc
        .get_block_hash(BLOCK_HEIGHT10)
        .await
        .expect("We should have a GetBlockHash struct");
    snapshot_rpc_getblockhash_valid(get_block_hash, &settings);

    let get_block_hash = get_block_template_rpc
        .get_block_hash(
            EXCESSIVE_BLOCK_HEIGHT
                .try_into()
                .expect("constant fits in i32"),
        )
        .await;
    snapshot_rpc_getblockhash_invalid("excessive_height", get_block_hash, &settings);

    // `getmininginfo`
    let get_mining_info = get_block_template_rpc
        .get_mining_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getmininginfo(get_mining_info, &settings);

    // `getblocksubsidy`
    let fake_future_block_height = fake_tip_height.0 + 100_000;
    let get_block_subsidy = get_block_template_rpc
        .get_block_subsidy(Some(fake_future_block_height))
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("future_height", get_block_subsidy, &settings);

    let get_block_subsidy = get_block_template_rpc
        .get_block_subsidy(None)
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("tip_height", get_block_subsidy, &settings);

    let get_block_subsidy = get_block_template_rpc
        .get_block_subsidy(Some(EXCESSIVE_BLOCK_HEIGHT))
        .await
        .expect("We should have a success response");
    snapshot_rpc_getblocksubsidy("excessive_height", get_block_subsidy, &settings);

    // `getpeerinfo`
    let get_peer_info = get_block_template_rpc
        .get_peer_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getpeerinfo(get_peer_info, &settings);

    // `getnetworksolps` (and `getnetworkhashps`)
    //
    // TODO: add tests for excessive num_blocks and height (#6688)
    //       add the same tests for get_network_hash_ps
    let get_network_sol_ps = get_block_template_rpc
        .get_network_sol_ps(None, None)
        .await
        .expect("We should have a success response");
    snapshot_rpc_getnetworksolps(get_network_sol_ps, &settings);

    // `getblocktemplate` - the following snapshots use a mock read_state

    // get a new empty state
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
                    history_tree: fake_history_tree(network),
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
                    // tip hash needs to match chain info for long poll requests
                    last_seen_tip_hash: fake_tip_hash,
                });
        }
    };

    // send tip hash and time needed for getblocktemplate rpc
    mock_chain_tip_sender.send_best_tip_hash(fake_tip_hash);

    // create a new rpc instance with new state and mock
    let get_block_template_rpc_mock_state = GetBlockTemplateRpcImpl::new(
        network,
        mining_config.clone(),
        Buffer::new(mempool.clone(), 1),
        read_state.clone(),
        mock_chain_tip.clone(),
        block_verifier_router,
        mock_sync_status.clone(),
        MockAddressBookPeers::default(),
    );

    // Basic variant (default mode and no extra features)

    // Fake the ChainInfo and FullTransaction responses
    let mock_read_state_request_handler = make_mock_read_state_request_handler();
    let mock_mempool_request_handler = make_mock_mempool_request_handler();

    let get_block_template_fut = get_block_template_rpc_mock_state.get_block_template(None);

    let (get_block_template, ..) = tokio::join!(
        get_block_template_fut,
        mock_mempool_request_handler,
        mock_read_state_request_handler,
    );

    let get_block_template::Response::TemplateMode(get_block_template) =
        get_block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!("this getblocktemplate call without parameters should return the `TemplateMode` variant of the response")
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

    let get_block_template_fut = get_block_template_rpc_mock_state.get_block_template(
        get_block_template::JsonParameters {
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

    let get_block_template::Response::TemplateMode(get_block_template) =
        get_block_template.expect("unexpected error in getblocktemplate RPC call")
    else {
        panic!("this getblocktemplate call without parameters should return the `TemplateMode` variant of the response")
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

    let get_block_template = get_block_template_rpc_mock_state.get_block_template(Some(
        get_block_template::JsonParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            data: Some(HexData("".into())),
            ..Default::default()
        },
    ));

    let get_block_template = get_block_template
        .await
        .expect("unexpected error in getblocktemplate RPC call");

    snapshot_rpc_getblocktemplate("invalid-proposal", get_block_template, None, &settings);

    // the following snapshots use a mock read_state and block_verifier_router

    let mut mock_block_verifier_router = MockService::build().for_unit_tests();
    let get_block_template_rpc_mock_state_verifier = GetBlockTemplateRpcImpl::new(
        network,
        mining_config,
        Buffer::new(mempool.clone(), 1),
        read_state.clone(),
        mock_chain_tip,
        mock_block_verifier_router.clone(),
        mock_sync_status,
        MockAddressBookPeers::default(),
    );

    let get_block_template_fut = get_block_template_rpc_mock_state_verifier.get_block_template(
        Some(get_block_template::JsonParameters {
            mode: GetBlockTemplateRequestMode::Proposal,
            data: Some(HexData(BLOCK_MAINNET_1_BYTES.to_vec())),
            ..Default::default()
        }),
    );

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

    let submit_block = get_block_template_rpc
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

    let validate_address = get_block_template_rpc
        .validate_address(founder_address.to_string())
        .await
        .expect("We should have a validate_address::Response");
    snapshot_rpc_validateaddress("basic", validate_address, &settings);

    let validate_address = get_block_template_rpc
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

    let z_validate_address = get_block_template_rpc
        .z_validate_address(founder_address.to_string())
        .await
        .expect("We should have a z_validate_address::Response");
    snapshot_rpc_z_validateaddress("basic", z_validate_address, &settings);

    let z_validate_address = get_block_template_rpc
        .z_validate_address("".to_string())
        .await
        .expect("We should have a z_validate_address::Response");
    snapshot_rpc_z_validateaddress("invalid", z_validate_address, &settings);

    // `getdifficulty`
    // This RPC snapshot uses both the mock and populated states

    // Fake the ChainInfo response using the mock state
    let mock_read_state_request_handler = make_mock_read_state_request_handler();

    let get_difficulty_fut = get_block_template_rpc_mock_state.get_difficulty();

    let (get_difficulty, ..) = tokio::join!(get_difficulty_fut, mock_read_state_request_handler,);

    let mock_get_difficulty = get_difficulty.expect("unexpected error in getdifficulty RPC call");

    snapshot_rpc_getdifficulty_valid("mock", mock_get_difficulty, &settings);

    // `z_listunifiedreceivers`

    let ua1 = String::from("u1l8xunezsvhq8fgzfl7404m450nwnd76zshscn6nfys7vyz2ywyh4cc5daaq0c7q2su5lqfh23sp7fkf3kt27ve5948mzpfdvckzaect2jtte308mkwlycj2u0eac077wu70vqcetkxf");
    let z_list_unified_receivers =
        tokio::spawn(get_block_template_rpc.z_list_unified_receivers(ua1))
            .await
            .expect("unexpected panic in z_list_unified_receivers RPC task")
            .expect("unexpected error in z_list_unified_receivers RPC call");

    snapshot_rpc_z_listunifiedreceivers("ua1", z_list_unified_receivers, &settings);

    let ua2 = String::from("u1uf4qsmh037x2jp6k042h9d2w22wfp39y9cqdf8kcg0gqnkma2gf4g80nucnfeyde8ev7a6kf0029gnwqsgadvaye9740gzzpmr67nfkjjvzef7rkwqunqga4u4jges4tgptcju5ysd0");
    let z_list_unified_receivers =
        tokio::spawn(get_block_template_rpc.z_list_unified_receivers(ua2))
            .await
            .expect("unexpected panic in z_list_unified_receivers RPC task")
            .expect("unexpected error in z_list_unified_receivers RPC call");

    snapshot_rpc_z_listunifiedreceivers("ua2", z_list_unified_receivers, &settings);
}

/// Snapshot `getblockcount` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockcount(block_count: u32, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_count", block_count));
}

/// Snapshot valid `getblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockhash_valid(block_hash: GetBlockHash, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_hash_valid", block_hash));
}

/// Snapshot invalid `getblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockhash_invalid(
    variant: &'static str,
    block_hash: Result<GetBlockHash>,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_hash_invalid_{variant}"), block_hash)
    });
}

/// Snapshot `getblocktemplate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblocktemplate(
    variant: &'static str,
    block_template: get_block_template::Response,
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
    submit_block_response: submit_block::Response,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!("snapshot_rpc_submit_block_invalid", submit_block_response)
    });
}

/// Snapshot `getmininginfo` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getmininginfo(
    get_mining_info: get_mining_info::Response,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("get_mining_info", get_mining_info));
}

/// Snapshot `getblocksubsidy` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblocksubsidy(
    variant: &'static str,
    get_block_subsidy: BlockSubsidy,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("get_block_subsidy_{variant}"), get_block_subsidy)
    });
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
    validate_address: validate_address::Response,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("validate_address_{variant}"), validate_address)
    });
}

/// Snapshot `z_validateaddress` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_z_validateaddress(
    variant: &'static str,
    z_validate_address: z_validate_address::Response,
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
    response: unified_address::Response,
    settings: &insta::Settings,
) {
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("z_list_unified_receivers_{variant}"), response)
    });
}
