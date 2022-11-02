//! Snapshot tests for getblocktemplate RPCs.
//!
//! To update these snapshots, run:
//! ```sh
//! cargo insta test --review --features getblocktemplate-rpcs --delete-unreferenced-snapshots
//! ```

use insta::Settings;
use tower::{buffer::Buffer, Service};

use zebra_chain::parameters::Network;
use zebra_node_services::mempool;
use zebra_state::LatestChainTip;

use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::methods::{
    get_block_template_rpcs::types::submit_block, GetBlockHash, GetBlockTemplateRpc,
    GetBlockTemplateRpcImpl,
};

pub async fn test_responses<State, ReadState>(
    network: Network,
    mempool: MockService<
        mempool::Request,
        mempool::Response,
        PanicAssertion,
        zebra_node_services::BoxError,
    >,
    state: State,
    read_state: ReadState,
    latest_chain_tip: LatestChainTip,
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

    let get_block_template_rpc = GetBlockTemplateRpcImpl::new(
        Buffer::new(mempool.clone(), 1),
        read_state,
        latest_chain_tip,
        chain_verifier,
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

    // `getblocktemplate`
    let get_block_template = tokio::spawn(get_block_template_rpc.get_block_template());

    mempool
        .expect_request(mempool::Request::FullTransactions)
        .await
        .respond(mempool::Response::FullTransactions(vec![]));

    let get_block_template = get_block_template
        .await
        .expect("unexpected panic in getblocktemplate RPC task")
        .expect("unexpected error in getblocktemplate RPC call");

    snapshot_rpc_getblocktemplate(get_block_template, &settings);

    // `submitblock`
    let submit_block = get_block_template_rpc
        .submit_block("".to_string(), None)
        .await;

    snapshot_rpc_submitblock(submit_block, &settings);
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
    block_template: crate::methods::get_block_template_rpcs::types::get_block_template::GetBlockTemplate,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("get_block_template", block_template));
}

/// Snapshot `submitblock` response, using `cargo insta` and JSON serialization.
fn snapshot_rpc_submitblock(
    submit_block_response: jsonrpc_core::Result<submit_block::Response>,
    settings: &insta::Settings,
) {
    settings.bind(|| insta::assert_json_snapshot!("submit_block", submit_block_response));
}
