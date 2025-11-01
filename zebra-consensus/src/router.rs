//! Top-level semantic block verification for Zebra.
//!
//! Verifies blocks using the [`CheckpointVerifier`] or full [`SemanticBlockVerifier`],
//! depending on the config and block height.
//!
//! # Correctness
//!
//! Block and transaction verification requests should be wrapped in a timeout, because:
//! - checkpoint verification waits for previous blocks, and
//! - full block and transaction verification wait for UTXOs from previous blocks.
//!
//! Otherwise, verification of out-of-order and invalid blocks and transactions can hang
//! indefinitely.

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
    parameters::{checkpoint::list::CheckpointList, Network},
};

use zebra_node_services::mempool;
use zebra_state as zs;

use crate::{
    block::{Request, SemanticBlockVerifier, VerifyBlockError},
    checkpoint::{CheckpointVerifier, VerifyCheckpointError},
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

/// The block verifier router routes requests to either the checkpoint verifier or the
/// semantic block verifier, depending on the maximum checkpoint height.
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
    /// The checkpointing block verifier.
    ///
    /// Always used for blocks before `Canopy`, optionally used for the entire checkpoint list.
    checkpoint: CheckpointVerifier<S>,

    /// The highest permitted checkpoint block.
    ///
    /// This height must be in the `checkpoint` verifier's checkpoint list.
    max_checkpoint_height: block::Height,

    /// The full semantic block verifier, used for blocks after `max_checkpoint_height`.
    block: SemanticBlockVerifier<S, V>,
}

/// An error while semantically verifying a block.
//
// One or both of these error variants are at least 140 bytes
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum RouterError {
    /// Block could not be checkpointed
    Checkpoint { source: Box<VerifyCheckpointError> },
    /// Block could not be full-verified
    Block { source: Box<VerifyBlockError> },
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            RouterError::Checkpoint { source } => {
                format!("block could not be checkpointed due to: {source}")
            }
            RouterError::Block { source } => {
                format!("block could not be full-verified due to: {source}")
            }
        })
    }
}

impl From<VerifyCheckpointError> for RouterError {
    fn from(err: VerifyCheckpointError) -> Self {
        RouterError::Checkpoint {
            source: Box::new(err),
        }
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
            RouterError::Checkpoint { source, .. } => source.is_duplicate_request(),
            RouterError::Block { source, .. } => source.is_duplicate_request(),
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        // TODO: Adjust these values based on zcashd (#9258).
        match self {
            RouterError::Checkpoint { source } => source.misbehavior_score(),
            RouterError::Block { source } => source.misbehavior_score(),
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
        // We acquire checkpoint readiness before block readiness, to avoid an unlikely
        // hang during the checkpoint to block verifier transition. If the checkpoint and
        // block verifiers are contending for the same buffer/batch, we want the checkpoint
        // verifier to win, so that checkpoint verification completes, and block verification
        // can start. (Buffers and batches have multiple slots, so this contention is unlikely.)
        use futures::ready;
        // The chain verifier holds one slot in each verifier, for each concurrent task.
        // Therefore, any shared buffers or batches polled by these verifiers should double
        // their bounds. (For example, the state service buffer.)
        ready!(self.checkpoint.poll_ready(cx))?;
        ready!(self.block.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let block = request.block();

        match block.coinbase_height() {
            // There's currently no known use case for block proposals below the checkpoint height,
            // so it's okay to immediately return an error here.
            Some(height) if height <= self.max_checkpoint_height && request.is_proposal() => {
                async {
                    // TODO: Add a `ValidateProposalError` enum with a `BelowCheckpoint` variant?
                    Err(VerifyBlockError::ValidateProposal(
                        "block proposals must be above checkpoint height".into(),
                    ))?
                }
                .boxed()
            }

            Some(height) if height <= self.max_checkpoint_height => {
                self.checkpoint.call(block).map_err(Into::into).boxed()
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
    mut state_service: S,
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
        // TODO: move this into an async function?
        async move {
            tracing::info!("starting state checkpoint validation");

            // # Consensus
            //
            // We want to verify all available checkpoints, even if the node is not configured
            // to use them for syncing. Zebra's checkpoints are updated with every release,
            // which makes sure they include the latest settled network upgrade.
            //
            // > A network upgrade is settled on a given network when there is a social
            // > consensus that it has activated with a given activation block hash.
            // > A full validator that potentially risks Mainnet funds or displays Mainnet
            // > transaction information to a user MUST do so only for a block chain that
            // > includes the activation block of the most recent settled network upgrade,
            // > with the corresponding activation block hash. Currently, there is social
            // > consensus that NU5 has activated on the Zcash Mainnet and Testnet with the
            // > activation block hashes given in § 3.12 ‘Mainnet and Testnet’ on p. 20.
            //
            // <https://zips.z.cash/protocol/protocol.pdf#blockchain>
            let full_checkpoints = checkpoint_network.checkpoint_list();
            let mut already_warned = false;

            for (height, checkpoint_hash) in full_checkpoints.iter() {
                let checkpoint_state_service = checkpoint_state_service.clone();
                let request = zebra_state::Request::BestChainBlockHash(*height);

                match checkpoint_state_service.oneshot(request).await {
                    Ok(zebra_state::Response::BlockHash(Some(state_hash))) => assert_eq!(
                        *checkpoint_hash, state_hash,
                        "invalid block in state: a previous Zebra instance followed an \
                         incorrect chain. Delete and re-sync your state to use the best chain"
                    ),

                    Ok(zebra_state::Response::BlockHash(None)) => {
                        if checkpoint_sync {
                            tracing::info!(
                                "state is not fully synced yet, remaining checkpoints will be \
                                 verified during syncing"
                            );
                        } else {
                            tracing::warn!(
                                "state is not fully synced yet, remaining checkpoints will be \
                                 verified next time Zebra starts up. Zebra will be less secure \
                                 until it is restarted. Use consensus.checkpoint_sync = true \
                                 in zebrad.toml to make sure you are following a valid chain"
                            );
                        }

                        break;
                    }

                    Ok(response) => {
                        unreachable!("unexpected response type: {response:?} from state request")
                    }
                    Err(e) => {
                        // This error happens a lot in some tests, and it could happen to users.
                        if !already_warned {
                            tracing::warn!(
                                "unexpected error: {e:?} in state request while verifying previous \
                                 state checkpoints. Is Zebra shutting down?"
                            );
                            already_warned = true;
                        }
                    }
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
    let (list, max_checkpoint_height) = init_checkpoint_list(config, network);

    let tip = match state_service
        .ready()
        .await
        .unwrap()
        .call(zs::Request::Tip)
        .await
        .unwrap()
    {
        zs::Response::Tip(tip) => tip,
        _ => unreachable!("wrong response to Request::Tip"),
    };
    tracing::info!(
        ?tip,
        ?max_checkpoint_height,
        "initializing block verifier router"
    );

    let block = SemanticBlockVerifier::new(network, state_service.clone(), transaction.clone());
    let checkpoint = CheckpointVerifier::from_checkpoint_list(list, network, tip, state_service);
    let router = BlockVerifierRouter {
        checkpoint,
        max_checkpoint_height,
        block,
    };

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
