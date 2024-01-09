//! Internal mining in Zebra.
//!
//! # TODO
//! - pause mining if we have no peers, like `zcashd` does,
//!   and add a developer config that mines regardless of how many peers we have.
//!   <https://github.com/zcash/zcash/blob/6fdd9f1b81d3b228326c9826fa10696fc516444b/src/miner.cpp#L865-L880>
//! - move common code into zebra-chain or zebra-node-services and remove the RPC dependency.

use std::{sync::Arc, time::Duration};

use color_eyre::Report;
use thread_priority::{ThreadBuilder, ThreadPriority};
use tokio::{select, sync::watch, task::JoinHandle, time::sleep};
use tower::Service;
use tracing::{Instrument, Span};

use zebra_chain::{
    block::{self, Block},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    diagnostic::task::WaitForPanics,
    serialization::ZcashSerialize,
    shutdown::is_shutting_down,
    work::equihash::{Solution, SolverCancelled},
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;
use zebra_rpc::{
    config::mining::Config,
    methods::{
        get_block_template_rpcs::{
            constants::GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
            get_block_template::{
                self, proposal::TimeSource, proposal_block_from_template,
                GetBlockTemplateCapability::*, GetBlockTemplateRequestMode::*,
            },
            types::hex_data::HexData,
        },
        GetBlockTemplateRpc, GetBlockTemplateRpcImpl,
    },
};
use zebra_state::WatchReceiver;

/// The amount of time we wait between block template retries.
pub const BLOCK_TEMPLATE_WAIT_TIME: Duration = Duration::from_secs(20);

/// Initialize the miner based on its config, and spawn a task for it.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core per configured
/// mining thread.
///
/// See [`run_mining_solver()`] for more details.
#[instrument(skip(config, rpc))]
pub fn spawn_init<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    config: &Config,
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> JoinHandle<Result<(), Report>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
/// See [`run_mining_solver()`] for more details.
pub async fn init<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    _config: Config,
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    let (template_sender, template_receiver) = watch::channel(None);
    let template_receiver = WatchReceiver::new(template_receiver);

    // TODO: add a config & launch the configured number of solvers, using available_parallelism()
    //       by default
    let template_generator = generate_block_templates(rpc.clone(), template_sender);
    let mining_solver = run_mining_solver(0, template_receiver, rpc);

    // These tasks run forever unless there is a fatal error or shutdown.
    // When that happens, the first task to notice returns, and the other tasks are cancelled.
    // Then this task returns and drops the `template_sender`, which cancels all the spawned miner
    // threads.
    select! {
        result = template_generator => { result?; }
        result = mining_solver => { result?; }
    }

    Ok(())
}

/// Generates block templates using `rpc`, and sends them to mining threads using `template_sender`.
#[instrument(skip(rpc, template_sender))]
pub async fn generate_block_templates<
    Mempool,
    State,
    Tip,
    BlockVerifierRouter,
    SyncStatus,
    AddressBook,
>(
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
    template_sender: watch::Sender<Option<Arc<Block>>>,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    let mut parameters = get_block_template::JsonParameters {
        mode: Template,
        data: None,
        capabilities: vec![LongPoll, CoinbaseTxn],
        long_poll_id: None,
        _work_id: None,
    };

    // Shut down the task when all the template receivers are dropped, or Zebra shuts down.
    while !template_sender.is_closed() && !is_shutting_down() {
        let template = rpc.get_block_template(Some(parameters.clone())).await;

        // Wait for the chain to sync so we get a valid template.
        let Ok(template) = template else {
            debug!(
                ?BLOCK_TEMPLATE_WAIT_TIME,
                "waiting for a valid block template",
            );

            // Skip the wait if we got an error because we are shutting down.
            if !is_shutting_down() {
                sleep(BLOCK_TEMPLATE_WAIT_TIME).await;
            }

            continue;
        };

        // Convert from RPC GetBlockTemplate to Block
        let template = template
            .try_into_template()
            .expect("invalid RPC response: proposal in response to a template request");

        info!(
            height = ?template.height,
            transactions = ?template.transactions.len(),
            "mining with an updated block template",
        );

        // Tell the next get_block_template() call to wait until the template has changed.
        parameters.long_poll_id = Some(template.long_poll_id);

        let block = proposal_block_from_template(&template, TimeSource::CurTime)
            .expect("unexpected invalid block template");

        // If the template has actually changed, send an updated template.
        template_sender.send_if_modified(|old_block| {
            if old_block.as_ref().map(|b| *b.header) == Some(*block.header) {
                return false;
            }
            *old_block = Some(Arc::new(block));
            true
        });

        // If the blockchain is changing rapidly, limit how often we'll update the template.
        // But if we're shutting down, do that immediately.
        if !template_sender.is_closed() && !is_shutting_down() {
            sleep(Duration::from_secs(
                GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL,
            ))
            .await;
        }
    }

    Ok(())
}

/// Runs a single mining thread that gets blocks from the `template_receiver`, calculates equihash
/// solutions with nonces based on `solver_id`, and submits valid blocks to Zebra's block validator.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core while running.
/// It can run for minutes or hours if the network difficulty is high. Mining uses a thread with
/// low CPU priority.
#[instrument(skip(template_receiver, rpc))]
pub async fn run_mining_solver<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    solver_id: u8,
    mut template_receiver: WatchReceiver<Option<Arc<Block>>>,
    rpc: GetBlockTemplateRpcImpl<Mempool, State, Tip, BlockVerifierRouter, SyncStatus, AddressBook>,
) -> Result<(), Report>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
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
    // Shut down the task when the template sender is dropped, or Zebra shuts down.
    while template_receiver.has_changed().is_ok() && !is_shutting_down() {
        // Get the latest block template, and mark the current value as seen.
        // We mark the value first to avoid missed updates.
        template_receiver.mark_as_seen();
        let template = template_receiver.cloned_watch_data();

        let Some(template) = template else {
            if solver_id == 0 {
                info!(
                    ?solver_id,
                    ?BLOCK_TEMPLATE_WAIT_TIME,
                    "solver waiting for initial block template"
                );
            } else {
                debug!(
                    ?solver_id,
                    ?BLOCK_TEMPLATE_WAIT_TIME,
                    "solver waiting for initial block template"
                );
            }

            // Skip the wait if we didn't get a template because we are shutting down.
            if !is_shutting_down() {
                sleep(BLOCK_TEMPLATE_WAIT_TIME).await;
            }

            continue;
        };

        let height = template.coinbase_height().expect("template is valid");

        // Set up the cancellation conditions for the miner.
        let mut cancel_receiver = template_receiver.clone();
        let old_header = *template.header;
        let cancel_fn = move || match cancel_receiver.has_changed() {
            // Despite the documentation, has_changed sometimes returns `true` spuriously, even
            // when the template hasn't changed. This could be a bug where the RPC or block
            // generator does spurious updates, or where the change detection is implemented
            // incorrectly. Since it's a bug in both the RPC and miner, it could be a tokio bug.
            Ok(has_changed) => {
                cancel_receiver.mark_as_seen();
                // We only need to check header equality, because the block data is bound to the
                // header.
                if has_changed
                    && Some(old_header) != cancel_receiver.cloned_watch_data().map(|b| *b.header)
                {
                    Err(SolverCancelled)
                } else {
                    Ok(())
                }
            }
            // If the sender was dropped, we're likely shutting down, so cancel the solver.
            Err(_sender_dropped) => Err(SolverCancelled),
        };

        // Mine a block using the equihash solver.
        let Ok(block) = mine_one_block(solver_id, template, cancel_fn).await else {
            // If the solver was cancelled, we're either shutting down, or we have a new template.
            if solver_id == 0 {
                info!(
                    ?height,
                    ?solver_id,
                    new_template = ?template_receiver.has_changed(),
                    shutting_down = ?is_shutting_down(),
                    "solver cancelled: getting a new block template or shutting down"
                );
            } else {
                debug!(
                    ?height,
                    ?solver_id,
                    new_template = ?template_receiver.has_changed(),
                    shutting_down = ?is_shutting_down(),
                    "solver cancelled: getting a new block template or shutting down"
                );
            }
            continue;
        };

        // Submit the newly mined block to the verifiers.
        //
        // TODO: if there is a new template (`cancel_fn().is_err()`), and
        //       GetBlockTemplate.submit_old is false, return immediately, and skip submitting the
        //       block.
        let data = block
            .zcash_serialize_to_vec()
            .expect("serializing to Vec never fails");

        match rpc.submit_block(HexData(data), None).await {
            Ok(success) => info!(
                ?height,
                hash = ?block.hash(),
                ?solver_id,
                ?success,
                "successfully mined a new block",
            ),
            Err(error) => info!(
                ?height,
                hash = ?block.hash(),
                ?solver_id,
                ?error,
                "validating a newly mined block failed, trying again",
            ),
        }
    }

    Ok(())
}

/// Mines a single block based on `template`, calculates its equihash solutions, checks difficulty,
/// and returning the newly mined block. Uses a different nonce range for each `solver_id`.
///
/// If `cancel_fn()` returns an error, returns early with `Err(SolverCancelled)`.
///
/// See [`run_mining_solver()`] for more details.
pub async fn mine_one_block<F>(
    solver_id: u8,
    mut template: Arc<Block>,
    cancel_fn: F,
) -> Result<Arc<Block>, SolverCancelled>
where
    F: FnMut() -> Result<(), SolverCancelled> + Send + Sync + 'static,
{
    // TODO: Replace with Arc::unwrap_or_clone() when it stabilises:
    // https://github.com/rust-lang/rust/issues/93610
    let mut header = *template.header;

    // Use a different nonce for each solver thread.
    // Change both the first and last bytes, so we don't have to care if the nonces are incremented in
    // big-endian or little-endian order. And we can see the thread that mined a block from the nonce.
    *header.nonce.first_mut().unwrap() = solver_id;
    *header.nonce.last_mut().unwrap() = solver_id;

    // Mine a block using the solver, in a low-priority blocking thread.
    let span = Span::current();
    // TODO: get and submit all valid headers, not just the first one
    let solved_header =
        tokio::task::spawn_blocking(move || span.in_scope(move || {
            let miner_thread_handle = ThreadBuilder::default().name("zebra-miner").priority(ThreadPriority::Min).spawn(move |priority_result| {
                if let Err(error) = priority_result {
                    info!(?error, "could not set miner to run at a low priority: running at default priority");
                }

                Solution::solve(header, cancel_fn)
            }).expect("unable to spawn miner thread");

            miner_thread_handle.wait_for_panics()
        }))
        .wait_for_panics()
        .await?;

    // Modify the template into a solved block.
    //
    // TODO: Replace with Arc::unwrap_or_clone() when it stabilises
    let block = Arc::make_mut(&mut template);
    block.header = Arc::new(solved_header);

    Ok(template)
}
