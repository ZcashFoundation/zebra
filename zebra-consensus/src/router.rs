//! Initialization of Zebra's block and transaction verification services.
//!
//! [`init`] builds the [`SemanticBlockVerifier`], which both performs full
//! semantic verification and gates commits at or below the known-hash floor:
//! on networks with a bundled known-hash list those heights are only ever
//! committed by the known-hash IBD engine, which checks them against the
//! pinned hash list (see `docs/design/known-hash-ibd.md` §7.2). Without the
//! gate, a gossiped or RPC-submitted block just above the engine's tip could
//! semantically verify mid-IBD and flip the state to its non-finalized mode,
//! silently demoting the rest of the initial sync.
//!
//! # Correctness
//!
//! Block and transaction verification requests should be wrapped in a
//! timeout, because full block and transaction verification wait for UTXOs
//! from previous blocks. Otherwise, verification of out-of-order and invalid
//! blocks and transactions can hang indefinitely.

use std::sync::Arc;

use tokio::{sync::oneshot, task::JoinHandle};
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::{instrument, Instrument, Span};

use zebra_chain::{
    block::{self, Height},
    parameters::{checkpoint::list::CheckpointList, known_hashes::KnownHashListSpec, Network},
};

use zebra_node_services::mempool;
use zebra_state as zs;

use crate::{
    block::{Request, SemanticBlockVerifier, VerifyBlockError},
    error::TransactionError,
    transaction, BoxError, Config,
};

pub mod service_trait;

#[cfg(test)]
mod tests;

/// The bound for the chain verifier and transaction verifier buffers.
///
/// We choose the verifier buffer bound based on the maximum number of
/// concurrent verifier users, to avoid contention:
///   - the `ChainSync` block download and verify stream
///   - the `Inbound` block download and verify stream
///   - the `Mempool` transaction download and verify stream
///   - a block miner component, which we might add in future, and
///   - 1 extra slot to avoid contention.
///
/// We deliberately add extra slots, because they only cost a small amount of
/// memory, but missing slots can significantly slow down Zebra.
const VERIFIER_BUFFER_BOUND: usize = 5;

/// Initialize block and transaction verification services.
///
/// Returns a block verifier, transaction verifier,
/// a [`BackgroundTaskHandles`] with the state checkpoint verify task,
/// and the maximum configured checkpoint verification height.
///
/// The consensus configuration is specified by `config`, and the Zcash network
/// to verify blocks for is specified by `network`.
///
/// The block verification service asynchronously performs semantic verification
/// checks. Blocks that pass semantic verification are submitted to the supplied
/// `state_service` for contextual verification before being committed to the chain.
///
/// The transaction verification service asynchronously performs semantic verification
/// checks. Transactions that pass semantic verification return an `Ok` result to the caller.
///
/// This function should only be called once for a particular state service.
///
/// Dropped requests are cancelled on a best-effort basis, but may continue to be processed.
///
/// # Correctness
///
/// Block and transaction verification requests should be wrapped in a timeout,
/// so that out-of-order and invalid requests do not hang indefinitely.
/// See the [`router`](`crate::router`) module documentation for details.
#[instrument(skip(state_service, mempool))]
pub async fn init<S, Mempool>(
    config: Config,
    network: &Network,
    state_service: S,
    mempool: oneshot::Receiver<Mempool>,
    known_hash_sync: bool,
) -> (
    Buffer<BoxService<Request, block::Hash, VerifyBlockError>, Request>,
    Buffer<
        BoxService<transaction::Request, transaction::Response, TransactionError>,
        transaction::Request,
    >,
    BackgroundTaskHandles,
    Height,
)
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    Mempool::Future: Send + 'static,
{
    // Give other tasks priority before spawning the checkpoint task.
    tokio::task::yield_now().await;

    // Make sure the state contains the known best chain checkpoints, in a separate thread.

    let checkpoint_state_service = state_service.clone();
    let checkpoint_network = network.clone();

    let state_checkpoint_verify_handle = tokio::task::spawn(
        async move {
            tracing::info!("starting state checkpoint validation");

            // # Consensus
            //
            // > A network upgrade is settled on a given network when there is a social
            // > consensus that it has activated with a given activation block hash.
            // > A full validator that potentially risks Mainnet funds or displays Mainnet
            // > transaction information to a user MUST do so only for a block chain that
            // > includes the activation block of the most recent settled network upgrade,
            // > with the corresponding activation block hash.
            //
            // <https://zips.z.cash/protocol/protocol.pdf#blockchain>
            //
            // An O(1) spot-check preserves the wrong-chain-on-restart
            // detection the old full-list walk provided, without issuing one
            // serial state request per checkpoint (design doc §7.2): the
            // highest checkpoint at or below the state tip pins the whole
            // chain below it by construction. The spot-check always runs (it
            // is a cheap consistency assertion), independent of the
            // `checkpoint_sync` config.
            let checkpoints = checkpoint_network.checkpoint_list();

            let tip = match checkpoint_state_service
                .clone()
                .oneshot(zebra_state::Request::Tip)
                .await
            {
                Ok(zebra_state::Response::Tip(tip)) => tip,
                Ok(response) => {
                    unreachable!("unexpected response type: {response:?} from state request")
                }
                Err(e) => {
                    tracing::warn!(
                        "unexpected error: {e:?} in state tip request while verifying previous \
                         state checkpoints. Is Zebra shutting down?"
                    );
                    return;
                }
            };

            let Some((tip_height, _)) = tip else {
                tracing::info!("state is empty, no previous chain to spot-check");
                return;
            };

            let Some(check_height) = checkpoints.max_height_in_range(..=tip_height) else {
                tracing::info!("no checkpoint at or below the state tip to spot-check");
                return;
            };

            let checkpoint_hash = checkpoints
                .hash(check_height)
                .expect("max_height_in_range returns a height in the list");

            match checkpoint_state_service
                .oneshot(zebra_state::Request::BestChainBlockHash(check_height))
                .await
            {
                Ok(zebra_state::Response::BlockHash(Some(state_hash))) => assert_eq!(
                    checkpoint_hash, state_hash,
                    "invalid block in state: a previous Zebra instance followed an \
                     incorrect chain. Delete and re-sync your state to use the best chain"
                ),
                Ok(zebra_state::Response::BlockHash(None)) => {
                    // The `Tip` and `BestChainBlockHash` reads are not atomic.
                    // The best chain can shrink below the spot-checked height
                    // between them — e.g. an `invalidateblock` RPC or a reorg
                    // to a shorter chain — leaving no block at that height. That
                    // is a benign race, not a wrong-chain indictment, so skip
                    // the spot-check this startup rather than panicking the
                    // consensus task (which would exit the node).
                    tracing::info!(
                        ?check_height,
                        "best chain changed during the startup spot-check; \
                         skipping it this run"
                    );
                }
                Ok(response) => {
                    unreachable!("unexpected response type: {response:?} from state request")
                }
                Err(e) => {
                    tracing::warn!(
                        "unexpected error: {e:?} in state request while verifying previous \
                         state checkpoints. Is Zebra shutting down?"
                    );
                }
            }

            tracing::info!("finished state checkpoint validation");
        }
        .instrument(Span::current()),
    );

    // transaction verification

    let transaction = transaction::Verifier::new(network, state_service.clone(), mempool);
    let transaction = Buffer::new(BoxService::new(transaction), VERIFIER_BUFFER_BOUND);

    // block verification
    let (_list, max_checkpoint_height) = init_checkpoint_list(config, network);

    // The commit-gate floor is built from constants (design doc §7.2). The
    // permanent floor is the mandatory checkpoint height — Zebra cannot fully
    // validate pre-Canopy blocks semantically, so it never commits them this
    // way.
    //
    // While the known-hash engine is enabled it owns the whole pinned range,
    // so the floor is raised to the list's max height: a gossiped or
    // RPC-submitted block just above the engine's tip must not semantically
    // verify mid-sync and flip the state to its non-finalized mode. When the
    // engine is disabled the floor stays at the mandatory height, so a node
    // syncing the post-Canopy range with the legacy path is not blocked by a
    // gate that exists only to protect the engine.
    let known_hash_floor = if known_hash_sync {
        KnownHashListSpec::for_network(network)
            .map_or(Height(0), |spec| spec.max_height)
            .max(network.mandatory_checkpoint_height())
    } else {
        network.mandatory_checkpoint_height()
    };

    tracing::info!(
        ?max_checkpoint_height,
        ?known_hash_floor,
        "initializing block verifier"
    );

    let block = SemanticBlockVerifier::new(
        network,
        state_service.clone(),
        transaction.clone(),
        known_hash_floor,
    );

    let block = Buffer::new(BoxService::new(block), VERIFIER_BUFFER_BOUND);

    let task_handles = BackgroundTaskHandles {
        state_checkpoint_verify_handle,
    };

    (block, transaction, task_handles, max_checkpoint_height)
}

/// Parses the checkpoint list for `network` and `config`.
/// Returns the checkpoint list and maximum checkpoint height.
pub fn init_checkpoint_list(config: Config, network: &Network) -> (Arc<CheckpointList>, Height) {
    // TODO: Zebra parses the checkpoint list three times at startup.
    //       Instead, cache the checkpoint list for each `network`.
    let list = network.checkpoint_list();

    let max_checkpoint_height = if config.checkpoint_sync {
        list.max_height()
    } else {
        list.min_height_in_range(network.mandatory_checkpoint_height()..)
            .expect("hardcoded checkpoint list extends past canopy activation")
    };

    (list, max_checkpoint_height)
}

/// The background task handles for `zebra-consensus` verifier initialization.
#[derive(Debug)]
pub struct BackgroundTaskHandles {
    /// A handle to the state checkpoint verify task.
    /// Finishes when all the checkpoints are verified, or when the state tip is reached.
    pub state_checkpoint_verify_handle: JoinHandle<()>,
}

/// Calls [`init`] with a closed mempool setup channel for conciseness in tests.
///
/// See [`init`] for more details.
#[cfg(any(test, feature = "proptest-impl"))]
pub async fn init_test<S>(
    config: Config,
    network: &Network,
    state_service: S,
) -> (
    Buffer<BoxService<Request, block::Hash, VerifyBlockError>, Request>,
    Buffer<
        BoxService<transaction::Request, transaction::Response, TransactionError>,
        transaction::Request,
    >,
    BackgroundTaskHandles,
    Height,
)
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    init(
        config.clone(),
        network,
        state_service.clone(),
        oneshot::channel::<
            Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>,
        >()
        .1,
        // Keep the gate at the known-hash list max for tests, matching the
        // default-on engine config.
        true,
    )
    .await
}
