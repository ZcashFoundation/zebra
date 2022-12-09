//! Snapshot tests for getblocktemplate RPCs.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --features getblocktemplate-rpcs --delete-unreferenced-snapshots
//! ```

use chrono::{TimeZone, Utc};
use hex::FromHex;
use insta::Settings;
use tower::{buffer::Buffer, Service};

use zebra_chain::{
    block::Hash,
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::MockChainTip,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
    transparent,
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
};
use zebra_node_services::mempool;

use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};

use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::methods::{
    get_block_template_rpcs::{
        self,
        types::{
            get_block_template::GetBlockTemplate, get_mining_info, hex_data::HexData, submit_block,
        },
    },
    tests::utils::fake_history_tree,
    GetBlockHash, GetBlockTemplateRpc, GetBlockTemplateRpcImpl,
};

pub async fn test_responses<State, ReadState>(
    network: Network,
    mut mempool: MockService<
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

    let mut mock_sync_status = MockSyncStatus::default();
    mock_sync_status.set_is_close_to_tip(true);

    let mining_config = get_block_template_rpcs::config::Config {
        miner_address: Some(transparent::Address::from_script_hash(network, [0xad; 20])),
    };

    // nu5 block height
    let fake_tip_height = NetworkUpgrade::Nu5.activation_height(network).unwrap();
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

    // get an rpc instance with continuous blockchain state
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        network,
        mining_config.clone(),
        Buffer::new(mempool.clone(), 1),
        read_state,
        mock_chain_tip.clone(),
        chain_verifier.clone(),
        mock_sync_status.clone(),
    );

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
    snapshot_rpc_getblockhash(get_block_hash, &settings);

    // `getmininginfo`
    let get_mining_info = get_block_template_rpc
        .get_mining_info()
        .await
        .expect("We should have a success response");
    snapshot_rpc_getmininginfo(get_mining_info, &settings);

    // `getnetworksolps` (and `getnetworkhashps`)
    let get_network_sol_ps = get_block_template_rpc
        .get_network_sol_ps(None, None)
        .await
        .expect("We should have a success response");
    snapshot_rpc_getnetworksolps(get_network_sol_ps, &settings);

    // get a new empty state
    let new_read_state = MockService::build().for_unit_tests();

    // send tip hash and time needed for getblocktemplate rpc
    mock_chain_tip_sender.send_best_tip_hash(fake_tip_hash);

    // create a new rpc instance with new state and mock
    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        network,
        mining_config,
        Buffer::new(mempool.clone(), 1),
        new_read_state.clone(),
        mock_chain_tip,
        chain_verifier,
        mock_sync_status,
    );

    // `getblocktemplate`

    // Fake the ChainInfo response
    tokio::spawn(async move {
        new_read_state
            .clone()
            .expect_request_that(|req| matches!(req, ReadRequest::ChainInfo))
            .await
            .respond(ReadResponse::ChainInfo(GetBlockTemplateChainInfo {
                expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
                tip_height: fake_tip_height,
                tip_hash: fake_tip_hash,
                cur_time: fake_cur_time,
                min_time: fake_min_time,
                max_time: fake_max_time,
                history_tree: fake_history_tree(network),
            }));
    });

    let get_block_template = tokio::spawn(get_block_template_rpc.get_block_template(None));

    mempool
        .expect_request(mempool::Request::FullTransactions)
        .await
        .respond(mempool::Response::FullTransactions(vec![]));

    let get_block_template = get_block_template
        .await
        .expect("unexpected panic in getblocktemplate RPC task")
        .expect("unexpected error in getblocktemplate RPC call");

    let coinbase_tx: Transaction = get_block_template
        .coinbase_txn
        .data
        .as_ref()
        .zcash_deserialize_into()
        .expect("coinbase bytes are valid");

    snapshot_rpc_getblocktemplate(get_block_template, coinbase_tx, &settings);

    // `submitblock`
    let submit_block = get_block_template_rpc
        .submit_block(HexData("".into()), None)
        .await
        .expect("unexpected error in submitblock RPC call");

    snapshot_rpc_submit_block_invalid(submit_block, &settings);
}

/// Snapshot `getblockcount` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockcount(block_count: u32, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_count", block_count));
}

/// Snapshot `getblockhash` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblockhash(block_hash: GetBlockHash, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_hash", block_hash));
}

/// Snapshot `getblocktemplate` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getblocktemplate(
    block_template: GetBlockTemplate,
    coinbase_tx: Transaction,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_template", block_template));
    settings.bind(|| insta::assert_json_snapshot!("get_block_template.coinbase_tx", coinbase_tx));
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

/// Snapshot `getnetworksolps` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_getnetworksolps(get_network_sol_ps: u64, settings: &insta::Settings) {
    settings.bind(|| insta::assert_json_snapshot!("get_network_sol_ps", get_network_sol_ps));
}
