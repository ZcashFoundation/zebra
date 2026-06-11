//! Top-level semantic block verification for Zebra.
//!
//! Verifies blocks using the full [`SemanticBlockVerifier`], gating commits
//! at or below the known-hash floor: on networks with a bundled known-hash
//! list, those heights are only ever committed by the known-hash IBD engine,
//! which checks them against the pinned hash list (see
//! `docs/design/known-hash-ibd.md` §7.2). Without the gate, a gossiped or
//! RPC-submitted block just above the engine's tip could semantically verify
//! mid-IBD and flip the state to its non-finalized mode, silently demoting
//! the rest of the initial sync.
//!
//! # Correctness
//!
//! Block and transaction verification requests should be wrapped in a
//! timeout, because full block and transaction verification wait for UTXOs
//! from previous blocks. Otherwise, verification of out-of-order and invalid
//! blocks and transactions can hang indefinitely.

use core::fmt;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{FutureExt, TryFutureExt};
use thiserror::Error;
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

/// The block verifier router gates commits at or below the known-hash floor
/// and routes everything else to the semantic block verifier.
///
/// # Correctness
///
/// Block verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`router`](`crate::router`)
/// module documentation for details.
struct BlockVerifierRouter<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<transaction::Request, Response = transaction::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    /// The commit-gate floor: the larger of the mandatory checkpoint height
    /// (Canopy − 1) and the network's known-hash list max height, when the
    /// network has a bundled list (design doc §7.2).
    ///
    /// Both inputs are constants, so the floor is fixed for the life of the
    /// process. Semantic commits at or below it are rejected: those blocks
    /// are only ever committed by the known-hash engine, and "Zebra cannot
    /// fully validate pre-Canopy blocks".
    floor: block::Height,

    /// The full semantic block verifier, used for blocks above the floor.
    block: SemanticBlockVerifier<S, V>,
}

/// An error while semantically verifying a block.
//
// One or both of these error variants are at least 140 bytes
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum RouterError {
    /// Block could not be full-verified
    Block { source: Box<VerifyBlockError> },
    /// The block is at or below the known-hash floor: those heights are only
    /// committed by the known-hash IBD engine, never by semantic
    /// verification (design doc §7.2). A benign rejection for gossiped
    /// blocks; an explicit one for RPC block submission.
    BelowKnownHashRange {
        /// The rejected block's height.
        height: block::Height,
        /// The floor in effect when the block was rejected.
        floor: block::Height,
    },
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            RouterError::Block { source } => {
                format!("block could not be full-verified due to: {source}")
            }
            RouterError::BelowKnownHashRange { height, floor } => format!(
                "block at height {height:?} is at or below the known-hash floor {floor:?}: \
                 blocks in that range are only committed by the known-hash sync engine"
            ),
        })
    }
}

impl From<VerifyBlockError> for RouterError {
    fn from(err: VerifyBlockError) -> Self {
        RouterError::Block {
            source: Box::new(err),
        }
    }
}

impl RouterError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        match self {
            RouterError::Block { source, .. } => source.is_duplicate_request(),
            RouterError::BelowKnownHashRange { .. } => false,
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        // TODO: Adjust these values based on zcashd (#9258).
        match self {
            RouterError::Block { source } => source.misbehavior_score(),
            // Peers cannot know our engine's exact floor timing, so a
            // below-floor block is not necessarily misbehavior.
            RouterError::BelowKnownHashRange { .. } => 0,
        }
    }
}

impl<S, V> Service<Request> for BlockVerifierRouter<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<transaction::Request, Response = transaction::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = RouterError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // If either verifier is unready, this task is scheduled for wakeup when it becomes
        // ready.
        //
        use futures::ready;
        ready!(self.block.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let block = request.block();

        let floor = self.floor;

        match block.coinbase_height() {
            // There's currently no known use case for block proposals below
            // the floor, so it's okay to immediately return an error here.
            Some(height) if height <= floor && request.is_proposal() => async {
                Err(VerifyBlockError::ValidateProposal(
                    "block proposals must be above the known-hash floor".into(),
                ))?
            }
            .boxed(),

            Some(height) if height <= floor => {
                async move { Err(RouterError::BelowKnownHashRange { height, floor }) }.boxed()
            }

            // This also covers blocks with no height, which the block verifier
            // will reject immediately.
            _ => self.block.call(request).map_err(Into::into).boxed(),
        }
    }
}

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
) -> (
    Buffer<BoxService<Request, block::Hash, RouterError>, Request>,
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
    let checkpoint_sync = config.checkpoint_sync;
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
            // chain below it by construction.
            let _ = checkpoint_sync;
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

    tracing::info!(?max_checkpoint_height, "initializing block verifier router");

    let block = SemanticBlockVerifier::new(network, state_service.clone(), transaction.clone());

    // The gate floor is built from constants: the mandatory checkpoint
    // height, raised to the known-hash list's max height on networks that
    // bundle a list (design doc §7.2).
    let floor = KnownHashListSpec::for_network(network)
        .map_or(Height(0), |spec| spec.max_height)
        .max(network.mandatory_checkpoint_height());

    let router = BlockVerifierRouter { floor, block };

    let router = Buffer::new(BoxService::new(router), VERIFIER_BUFFER_BOUND);

    let task_handles = BackgroundTaskHandles {
        state_checkpoint_verify_handle,
    };

    (router, transaction, task_handles, max_checkpoint_height)
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
    Buffer<BoxService<Request, block::Hash, RouterError>, Request>,
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
    )
    .await
}
