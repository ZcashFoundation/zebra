//! Internal mining in Zebra.
//!
//! # TODO
//! - pause mining if we have no peers, like `zcashd` does,
//!   and add a developer config that mines regardless of how many peers we have.
//!   <https://github.com/zcash/zcash/blob/6fdd9f1b81d3b228326c9826fa10696fc516444b/src/miner.cpp#L865-L880>
//! - move common code into zebra-chain or zebra-node-services and remove the RPC dependency.

use std::sync::Arc;

use color_eyre::Report;
use tokio::task::JoinHandle;
use tower::Service;
use tracing::{Instrument, Span};

use zebra_chain::{
    block::{self, Block},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    diagnostic::task::WaitForPanics,
    serialization::ZcashSerialize,
    work::equihash::Solution,
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;
use zebra_rpc::{
    config::mining::Config,
    methods::{
        get_block_template_rpcs::{
            get_block_template::{
                self, proposal::TimeSource, proposal_block_from_template, GetBlockTemplate,
                GetBlockTemplateCapability::*, GetBlockTemplateRequestMode::*,
            },
            types::hex_data::HexData,
        },
        GetBlockTemplateRpc, GetBlockTemplateRpcImpl,
    },
};

/// Initialize the miner based on its config, and spawn a task for it.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core per configured
/// mining thread.
///
/// TODO: add a test for this function.
pub fn spawn_init<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    config: &Config,
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> JoinHandle<Result<(), Report>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    let config = config.clone();

    // TODO: spawn an entirely new executor here, so mining is isolated from higher priority tasks.
    tokio::spawn(init(config, rpc).in_current_span())
}

/// Initialize the miner based on its config.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core per configured
/// mining thread.
///
/// TODO: add a test for this function.
pub async fn init<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    _config: Config,
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    // TODO: launch the configured number of solvers
    //       make mining threads lower priority than other threads
    run_mining_solver(rpc, 0).await
}

/// Runs a single mining thread to generate blocks, calculate equihash solutions, and submit valid
/// blocks to Zebra's block validator.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core while running.
/// It can run for minutes or hours if the network difficulty is high.
#[instrument(skip(rpc))]
pub async fn run_mining_solver<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
    thread_id: usize,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <State as Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    BlockVerifierRouter: Service<zebra_consensus::Request, Response = block::Hash, Error = zebra_consensus::BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
{
    // Pass the correct arguments, even if Zebra currently ignores them.
    let mut long_poll_id = None;
    let mut parameters = get_block_template::JsonParameters {
        mode: Template,
        data: None,
        capabilities: vec![LongPoll, CoinbaseTxn],
        long_poll_id,
        _work_id: None,
    };

    let template = rpc.get_block_template(Some(parameters)).await?;
    let template = template
        .try_into_template()
        .expect("invalid RPC response: proposal in response to a template request");

    // TODO: select!{} on either a solved header or a new block template using long_poll_id
    //       cancel the solver if there's a new template
    //       use a different nonce for each solver thread
    let block = mine_one_block(template, thread_id).await?;
    let data = block
        .zcash_serialize_to_vec()
        .expect("serializing to Vec never fails");

    match rpc.submit_block(HexData(data), None).await {
        Ok(success) => info!(
            height = ?block.coinbase_height().expect("height checked by verifier"),
            hash = ?block.hash(),
            ?thread_id,
            ?success,
            "successfully mined a new block",
        ),
        Err(error) => info!(
            height = ?block.coinbase_height().expect("height valid in template"),
            hash = ?block.hash(),
            ?thread_id,
            ?error,
            "validating a newly mined block failed, trying again",
        ),
    }

    Ok(())
}

/// Mines a single block, calculating its equihash solutions, and submitting it to Zebra's block
/// validator.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core while running.
/// It can run for minutes or hours if the network difficulty is high.
#[instrument(skip(template))]
pub async fn mine_one_block(template: GetBlockTemplate, thread_id: usize) -> Result<Block, Report> {
    let mut block = proposal_block_from_template(&template, TimeSource::CurTime)?;

    // TODO: spawn to a low priority thread
    //
    // TODO: Replace with Arc::unwrap_or_clone() when it stabilises:
    // https://github.com/rust-lang/rust/issues/93610
    let span = Span::current();
    let solved_header =
        tokio::task::spawn_blocking(move || span.in_scope(move || Solution::solve(*block.header)))
            .wait_for_panics()
            .await;

    block.header = Arc::new(solved_header);

    Ok(block)
}
