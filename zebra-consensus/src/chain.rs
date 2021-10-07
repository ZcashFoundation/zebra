//! Top-level semantic block verification for Zebra.
//!
//! Verifies blocks using the [`CheckpointVerifier`] or full [`BlockVerifier`],
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

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use displaydoc::Display;
use futures::{FutureExt, TryFutureExt};
use thiserror::Error;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use zebra_state as zs;

use crate::{
    block::BlockVerifier,
    block::VerifyBlockError,
    checkpoint::{CheckpointList, CheckpointVerifier, VerifyCheckpointError},
    error::TransactionError,
    script, transaction, BoxError, Config,
};

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

/// The chain verifier routes requests to either the checkpoint verifier or the
/// block verifier, depending on the maximum checkpoint height.
///
/// # Correctness
///
/// Block verification requests should be wrapped in a timeout, so that
/// out-of-order and invalid requests do not hang indefinitely. See the [`chain`](`crate::chain`)
/// module documentation for details.
struct ChainVerifier<S, V>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    V: Service<transaction::Request, Response = transaction::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    block: BlockVerifier<S, V>,
    checkpoint: CheckpointVerifier<S>,
    max_checkpoint_height: block::Height,
}

/// An error while semantically verifying a block.
#[derive(Debug, Display, Error)]
pub enum VerifyChainError {
    /// block could not be checkpointed
    Checkpoint(#[source] VerifyCheckpointError),
    /// block could not be verified
    Block(#[source] VerifyBlockError),
}

impl<S, V> Service<Arc<Block>> for ChainVerifier<S, V>
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
    type Error = VerifyChainError;
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
        ready!(self
            .checkpoint
            .poll_ready(cx)
            .map_err(VerifyChainError::Checkpoint))?;
        ready!(self.block.poll_ready(cx).map_err(VerifyChainError::Block))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        match block.coinbase_height() {
            Some(height) if height <= self.max_checkpoint_height => self
                .checkpoint
                .call(block)
                .map_err(VerifyChainError::Checkpoint)
                .boxed(),
            // This also covers blocks with no height, which the block verifier
            // will reject immediately.
            _ => self
                .block
                .call(block)
                .map_err(VerifyChainError::Block)
                .boxed(),
        }
    }
}

/// Initialize block and transaction verification services.
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
/// See the [`chain`](`crate::chain`) module documentation for details.
#[instrument(skip(state_service))]
pub async fn init<S>(
    config: Config,
    network: Network,
    mut state_service: S,
) -> (
    Buffer<BoxService<Arc<Block>, block::Hash, VerifyChainError>, Arc<Block>>,
    Buffer<
        BoxService<transaction::Request, transaction::Response, TransactionError>,
        transaction::Request,
    >,
)
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    // transaction verification

    let script = script::Verifier::new(state_service.clone());
    let transaction = transaction::Verifier::new(network, script);
    let transaction = Buffer::new(BoxService::new(transaction), VERIFIER_BUFFER_BOUND);

    // block verification

    let list = CheckpointList::new(network);

    let max_checkpoint_height = if config.checkpoint_sync {
        list.max_height()
    } else {
        list.min_height_in_range(network.mandatory_checkpoint_height()..)
            .expect("hardcoded checkpoint list extends past canopy activation")
    };

    let tip = match state_service
        .ready_and()
        .await
        .unwrap()
        .call(zs::Request::Tip)
        .await
        .unwrap()
    {
        zs::Response::Tip(tip) => tip,
        _ => unreachable!("wrong response to Request::Tip"),
    };
    tracing::info!(?tip, ?max_checkpoint_height, "initializing chain verifier");

    let block = BlockVerifier::new(network, state_service.clone(), transaction.clone());
    let checkpoint = CheckpointVerifier::from_checkpoint_list(list, network, tip, state_service);
    let chain = ChainVerifier {
        block,
        checkpoint,
        max_checkpoint_height,
    };

    let chain = Buffer::new(BoxService::new(chain), VERIFIER_BUFFER_BOUND);

    (chain, transaction)
}
