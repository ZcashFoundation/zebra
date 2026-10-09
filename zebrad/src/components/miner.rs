//! Internal mining in Zebra.
//!
//! # TODO
//! - move common code into zebra-chain or zebra-node-services and remove the RPC dependency.

use std::{cmp::min, sync::Arc, thread::available_parallelism, time::Duration};

use color_eyre::Report;
use futures::{stream::FuturesUnordered, StreamExt};
use thread_priority::{ThreadBuilder, ThreadPriority};
use tokio::{select, sync::watch, task::JoinHandle, time::sleep};
use tower::Service;
use tracing::{Instrument, Span};

use zebra_chain::{
    block::{self, Block},
    chain_sync_status::ChainSyncStatus,
    chain_tip::ChainTip,
    diagnostic::task::WaitForPanics,
    serialization::{AtLeastOne, ZcashSerialize},
    shutdown::is_shutting_down,
    work::equihash::{Solution, SolverCancelled},
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;
use zebra_rpc::{
    client::{
        BlockTemplateTimeSource,
        GetBlockTemplateCapability::{CoinbaseTxn, LongPoll},
        GetBlockTemplateParameters,
        GetBlockTemplateRequestMode::Template,
        HexData,
    },
    methods::{RpcImpl, RpcServer},
    proposal_block_from_template,
};
use zebra_state::WatchReceiver;

use zebra_chain::parameters::Network;
use zebra_rpc::{
    client::{SubmitBlockErrorResponse, SubmitBlockResponse},
    config::mining::Config,
};

#[cfg(test)]
mod tests;

/// Keeps useful solver work across mempool-only updates.
fn should_replace_mining_template(
    current_header: Option<block::Header>,
    new_header: block::Header,
    submit_old: Option<bool>,
) -> bool {
    current_header != Some(new_header) && (current_header.is_none() || submit_old != Some(true))
}

/// Mainnet never permits private mining, even if the Testnet-only option is enabled.
fn is_private_mining(network: &Network, config: &Config) -> bool {
    network.is_regtest()
        || (!matches!(network, Network::Mainnet) && config.internal_miner_private_testnet)
}

/// One unresolved local submission is enough to prevent a self-amplifying private fork.
#[derive(Clone, Copy, Debug)]
struct SubmittedBlock {
    hash: block::Hash,
    height: block::Height,
}

impl SubmittedBlock {
    /// Never extend an uncommitted local parent; public networks also wait for another miner.
    ///
    /// An inconclusive or transport response can precede admission. Until the public chain has
    /// reached this height, do not submit additional siblings or forget the submitted identity.
    fn permits_work(
        self,
        parent: block::Hash,
        committed: Option<(block::Height, block::Hash)>,
        private: bool,
    ) -> bool {
        let Some((height, hash)) = committed else {
            return false;
        };
        height >= self.height && (parent != self.hash || (private && hash == self.hash))
    }
}

/// Only a definitive invalid-block verdict releases an unresolved submission.
fn submission_was_rejected(response: &Result<SubmitBlockResponse, impl std::fmt::Debug>) -> bool {
    matches!(
        response,
        Ok(SubmitBlockResponse::ErrorResponse(
            SubmitBlockErrorResponse::Rejected
        ))
    )
}

/// Checks both the published work and its parent, including after the solver returns.
fn cancel_if_mining_template_changed(
    template_receiver: &WatchReceiver<Option<Arc<Block>>>,
    mining_tip: &watch::Receiver<Option<(block::Height, block::Hash)>>,
    old_header: block::Header,
    eligible: bool,
) -> Result<(), SolverCancelled> {
    if template_receiver.has_changed().is_err()
        || mining_tip.has_changed().is_err()
        || template_receiver.cloned_watch_data().map(|b| *b.header) != Some(old_header)
        || mining_tip.borrow().map(|(_, hash)| hash) != Some(old_header.previous_block_hash)
        || !eligible
    {
        Err(SolverCancelled)
    } else {
        Ok(())
    }
}

/// Bounds retries without delaying new work or shutdown.
async fn wait_for_mining_tip_change(
    mining_tip: &mut watch::Receiver<Option<(block::Height, block::Hash)>>,
    delay: Duration,
) -> Result<bool, watch::error::RecvError> {
    tokio::select! {
        biased;
        result = mining_tip.changed() => result.map(|()| true),
        _ = sleep(delay) => Ok(false),
    }
}

/// Parks an idle solver until a template arrives, periodically checking shutdown.
async fn wait_for_mining_template_change(
    template_receiver: &mut WatchReceiver<Option<Arc<Block>>>,
) {
    tokio::select! {
        _ = template_receiver.changed() => {},
        _ = sleep(BLOCK_TEMPLATE_WAIT_TIME) => {},
    }
}

/// The amount of time we wait between block template retries.
pub const BLOCK_TEMPLATE_WAIT_TIME: Duration = Duration::from_secs(20);

/// A rate-limit for block template refreshes.
pub const BLOCK_TEMPLATE_REFRESH_LIMIT: Duration = Duration::from_secs(2);

/// How long we wait after mining a block, before expecting a new template.
///
/// This should be slightly longer than `BLOCK_TEMPLATE_REFRESH_LIMIT` to allow for template
/// generation.
pub const BLOCK_MINING_WAIT_TIME: Duration = Duration::from_secs(3);

/// Initialize the miner based on its config, and spawn a task for it.
///
/// Uses the mining policy in `config`; the RPC supplies current peer, sync, and committed-tip data.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core per configured
/// mining thread.
///
/// See [`run_mining_solver()`] for more details.
pub fn spawn_init<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>(
    config: &Config,
    rpc: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
) -> JoinHandle<Result<(), Report>>
// TODO: simplify or avoid repeating these generics (how?)
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
    // TODO: spawn an entirely new executor here, so mining is isolated from higher priority tasks.
    tokio::spawn(init(config.clone(), rpc).in_current_span())
}

/// Initialize the miner based on its config.
///
/// Public work requires live peers and synchronization. Private Testnet requires explicit opt-in;
/// Regtest is inherently private. Mainnet always retains the public policy.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core per configured
/// mining thread.
///
/// See [`run_mining_solver()`] for more details.
pub async fn init<Mempool, State, ReadState, Tip, BlockVerifierRouter, SyncStatus, AddressBook>(
    config: Config,
    rpc: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
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
    let configured_threads = 1;
    // If we can't detect the number of cores, use the configured number.
    let available_threads = available_parallelism()
        .map(usize::from)
        .unwrap_or(configured_threads);

    // Use the minimum of the configured and available threads.
    let solver_count = min(configured_threads, available_threads);

    info!(
        ?solver_count,
        "launching mining tasks with parallel solvers"
    );

    let (template_sender, template_receiver) = watch::channel(None);
    let template_receiver = WatchReceiver::new(template_receiver);

    // Spawn these tasks, to avoid blocked cooperative futures, and improve shutdown responsiveness.
    // This is particularly important when there are a large number of solver threads.
    let mut abort_handles = Vec::new();

    let template_generator = tokio::task::spawn(
        generate_block_templates(config.clone(), rpc.clone(), template_sender).in_current_span(),
    );
    abort_handles.push(template_generator.abort_handle());
    let template_generator = template_generator.wait_for_panics();

    let mut mining_solvers = FuturesUnordered::new();
    for solver_id in 0..solver_count {
        // Assume there are less than 256 cores. If there are more, only run 256 tasks.
        let solver_id = min(solver_id, usize::from(u8::MAX))
            .try_into()
            .expect("just limited to u8::MAX");

        let solver = tokio::task::spawn(
            run_mining_solver(
                solver_id,
                config.clone(),
                template_receiver.clone(),
                rpc.clone(),
            )
            .in_current_span(),
        );
        abort_handles.push(solver.abort_handle());

        mining_solvers.push(solver.wait_for_panics());
    }

    // These tasks run forever unless there is a fatal error or shutdown.
    // When that happens, the first task to error returns, and the other JoinHandle futures are
    // cancelled.
    let first_result;
    select! {
        result = template_generator => { first_result = result; }
        result = mining_solvers.next() => {
            first_result = result
                .expect("stream never terminates because there is at least one solver task");
        }
    }

    // But the spawned async tasks keep running, so we need to abort them here.
    for abort_handle in abort_handles {
        abort_handle.abort();
    }

    // Any spawned blocking threads will keep running. When this task returns and drops the
    // `template_sender`, it cancels all the spawned miner threads. This works because we've
    // aborted the `template_generator` task, which owns the `template_sender`. (And it doesn't
    // spawn any blocking threads.)
    first_result
}

/// Generates block templates using `rpc`, and sends them to mining threads using `template_sender`.
///
/// Public work is withheld without synchronization and recently live peers. Private Testnet and
/// Regtest use the same prepared-work long polling without public synchronization prerequisites.
#[instrument(skip(rpc, template_sender))]
pub async fn generate_block_templates<
    Mempool,
    State,
    ReadState,
    Tip,
    BlockVerifierRouter,
    SyncStatus,
    AddressBook,
>(
    config: Config,
    rpc: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
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
    let mut parameters =
        GetBlockTemplateParameters::new(Template, None, vec![LongPoll, CoinbaseTxn], None, None);
    let mut mining_tip = rpc.mining_tip_change().await?.receiver;
    let mut private_long_poll_id = None;

    // Shut down the task when all the template receivers are dropped, or Zebra shuts down.
    while !template_sender.is_closed() && !is_shutting_down() {
        mining_tip.borrow_and_update();
        let template = if is_private_mining(rpc.network(), &config) {
            rpc.private_mining_template(private_long_poll_id).await
        } else if rpc.public_mining_is_eligible() {
            rpc.get_block_template(Some(parameters.clone()))
                .await
                .map(|response| {
                    response
                        .try_into_template()
                        .expect("invalid RPC response: proposal in response to a template request")
                })
        } else {
            template_sender.send_if_modified(|template| template.take().is_some());
            wait_for_mining_tip_change(&mut mining_tip, BLOCK_TEMPLATE_REFRESH_LIMIT).await?;
            continue;
        };

        // Wait for the chain to sync so we get a valid template.
        let Ok(template) = template else {
            template_sender.send_if_modified(|template| template.take().is_some());
            warn!(
                ?BLOCK_TEMPLATE_WAIT_TIME,
                ?template,
                "waiting for a valid block template",
            );

            // Skip the wait if we got an error because we are shutting down.
            if !is_shutting_down() {
                wait_for_mining_tip_change(&mut mining_tip, BLOCK_TEMPLATE_WAIT_TIME).await?;
            }

            continue;
        };

        let submit_old = template.submit_old();
        private_long_poll_id = Some(template.long_poll_id());

        info!(
            height = ?template.height(),
            transactions = ?template.transactions().len(),
            "mining with an updated block template",
        );

        // Tell the next get_block_template() call to wait until the template has changed.
        parameters = GetBlockTemplateParameters::new(
            Template,
            None,
            vec![LongPoll, CoinbaseTxn],
            Some(template.long_poll_id()),
            None,
        );

        let block = proposal_block_from_template(
            &template,
            BlockTemplateTimeSource::CurTime,
            rpc.network(),
        )?;

        // If the template has actually changed, send an updated template.
        template_sender.send_if_modified(|old_block| {
            if !should_replace_mining_template(
                old_block.as_ref().map(|b| *b.header),
                *block.header,
                submit_old,
            ) {
                return false;
            }
            *old_block = Some(Arc::new(block));
            true
        });

        // Mempool-only refreshes stay rate-limited; parent changes interrupt the cooldown.
        if !template_sender.is_closed()
            && !is_shutting_down()
            && wait_for_mining_tip_change(&mut mining_tip, BLOCK_TEMPLATE_REFRESH_LIMIT).await?
        {
            let parent = mining_tip.borrow().map(|(_, hash)| hash);
            template_sender.send_if_modified(|template| {
                if template
                    .as_ref()
                    .map(|block| block.header.previous_block_hash)
                    == parent
                {
                    return false;
                }
                template.take().is_some()
            });
        }
    }

    Ok(())
}

/// Runs a single mining thread that gets blocks from the `template_receiver`, calculates equihash
/// solutions with nonces based on `solver_id`, and submits valid blocks to Zebra's block validator.
///
/// Cancels on parent/template changes and loss of public eligibility, including before submission.
/// Remembers one local submission before awaiting its result, and never extends an uncommitted
/// local parent. Public mining also refuses to extend a committed block from this process.
///
/// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core while running.
/// It can run for minutes or hours if the network difficulty is high. Mining uses a thread with
/// low CPU priority.
#[instrument(skip(template_receiver, rpc))]
pub async fn run_mining_solver<
    Mempool,
    State,
    ReadState,
    Tip,
    BlockVerifierRouter,
    SyncStatus,
    AddressBook,
>(
    solver_id: u8,
    config: Config,
    mut template_receiver: WatchReceiver<Option<Arc<Block>>>,
    rpc: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
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
    let mining_tip = rpc.mining_tip_change().await?.receiver;
    let private = is_private_mining(rpc.network(), &config);
    let mut submitted: Option<SubmittedBlock> = None;
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
                wait_for_mining_template_change(&mut template_receiver).await;
            }

            continue;
        };

        let height = template.coinbase_height().expect("template is valid");

        if (!private && !rpc.public_mining_is_eligible())
            || submitted.is_some_and(|submitted| {
                !submitted.permits_work(
                    template.header.previous_block_hash,
                    rpc.committed_mining_tip(),
                    private,
                )
            })
        {
            tokio::select! {
                _ = template_receiver.changed() => {},
                _ = sleep(BLOCK_TEMPLATE_REFRESH_LIMIT) => {},
            }
            continue;
        }

        // Set up the cancellation conditions for the miner.
        let cancel_receiver = template_receiver.clone();
        let cancel_tip = mining_tip.clone();
        let old_header = *template.header;
        let cancel_rpc = rpc.clone();
        let cancel_fn = move || {
            cancel_if_mining_template_changed(
                &cancel_receiver,
                &cancel_tip,
                old_header,
                private || cancel_rpc.public_mining_is_eligible(),
            )
        };

        // Mine at least one block using the equihash solver.
        let Ok(blocks) = mine_a_block(solver_id, template, rpc.network().clone(), cancel_fn).await
        else {
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
            // A parent change can cancel before replacement work is published.
            // Park rather than repeatedly spawning already-cancelled solver threads.
            if matches!(template_receiver.has_changed(), Ok(false)) && !is_shutting_down() {
                wait_for_mining_template_change(&mut template_receiver).await;
            }

            continue;
        };

        // Recheck after solving: a parent/template can change after the solver's last cancellation
        // check. Check each solution, since submitting the previous one can also change the tip.
        let mut any_success = false;
        for block in blocks {
            if cancel_if_mining_template_changed(
                &template_receiver,
                &mining_tip,
                old_header,
                private || rpc.public_mining_is_eligible(),
            )
            .is_err()
                || is_shutting_down()
                || submitted.is_some_and(|submitted| {
                    !submitted.permits_work(
                        old_header.previous_block_hash,
                        rpc.committed_mining_tip(),
                        private,
                    )
                })
            {
                break;
            }
            let data = block
                .zcash_serialize_to_vec()
                .expect("serializing to Vec never fails");

            // Publish the local identity before awaiting: admission can change the mining parent
            // before submitblock responds, including on an inconclusive or transport outcome.
            submitted = Some(SubmittedBlock {
                hash: block.hash(),
                height,
            });
            let response = rpc.submit_block(HexData(data), None).await;
            if submission_was_rejected(&response) {
                submitted = None;
            }
            match response {
                Ok(SubmitBlockResponse::Accepted) => {
                    info!(?height, hash = ?block.hash(), ?solver_id, "successfully mined a new block");
                    any_success = true;
                }
                response => info!(
                    ?height,
                    hash = ?block.hash(),
                    ?solver_id,
                    ?response,
                    "mined block was not confirmed accepted",
                ),
            }
            // A non-rejected result might still commit later. Keep only one pending submission.
            if submitted.is_some() {
                break;
            }
        }

        // Start re-mining quickly after a failed solution.
        // If there's a new template, we'll use it, otherwise the existing one is ok.
        if !any_success {
            if matches!(template_receiver.has_changed(), Ok(false)) && !is_shutting_down() {
                tokio::select! {
                    _ = template_receiver.changed() => {},
                    _ = sleep(BLOCK_TEMPLATE_REFRESH_LIMIT) => {},
                }
            }
            continue;
        }

        // Wait for the new block to verify, and the RPC task to pick up a new template.
        // But don't wait too long, we could have mined on a fork.
        tokio::select! {
            shutdown_result = template_receiver.changed() => shutdown_result?,
            _ = sleep(BLOCK_MINING_WAIT_TIME) => {}

        }
    }

    Ok(())
}

/// Mines one or more blocks based on `template`. Calculates equihash solutions, checks difficulty,
/// and returns as soon as it has at least one block. Uses a different nonce range for each
/// `solver_id`.
///
/// If `cancel_fn()` returns an error, returns early with `Err(SolverCancelled)`.
///
/// Regtest returns a null-solution candidate without production Equihash solving; submission still
/// performs the network's normal verification. Mainnet and Testnet retain the production solver.
///
/// See [`run_mining_solver()`] for more details.
pub async fn mine_a_block<F>(
    solver_id: u8,
    template: Arc<Block>,
    network: Network,
    mut cancel_fn: F,
) -> Result<AtLeastOne<Block>, SolverCancelled>
where
    F: FnMut() -> Result<(), SolverCancelled> + Send + Sync + 'static,
{
    let mut header = *template.header;

    // Use a different nonce for each solver thread.
    // Change both the first and last bytes, so we don't have to care if the nonces are incremented in
    // big-endian or little-endian order. And we can see the thread that mined a block from the nonce.
    *header.nonce.first_mut().unwrap() = solver_id;
    *header.nonce.last_mut().unwrap() = solver_id;

    // Regtest follows generate's null-solution path, never the production Tromp(200,9) solver.
    if network.is_regtest() {
        cancel_fn()?;
        if is_shutting_down() {
            return Err(SolverCancelled);
        }
        header.solution = Solution::Regtest([0; 36]);
        let mut block = (*template).clone();
        block.header = Arc::new(header);
        return Ok(vec![block]
            .try_into()
            .expect("one Regtest candidate is nonempty"));
    }

    // Mine one or more blocks using the solver, in a low-priority blocking thread.
    let span = Span::current();
    let solved_headers =
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

    // Modify the template into solved blocks.

    // TODO: Replace with Arc::unwrap_or_clone() when it stabilises
    let block = (*template).clone();

    let solved_blocks: Vec<Block> = solved_headers
        .into_iter()
        .map(|header| {
            let mut block = block.clone();
            block.header = Arc::new(header);
            block
        })
        .collect();

    Ok(solved_blocks
        .try_into()
        .expect("a 1:1 mapping of AtLeastOne produces at least one block"))
}
