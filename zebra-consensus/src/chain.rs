#[cfg(test)]
mod tests;

use displaydoc::Display;
use futures::{FutureExt, TryFutureExt};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use thiserror::Error;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade::Sapling},
};

use zebra_state as zs;

use crate::{
    block::BlockVerifier,
    block::VerifyBlockError,
    checkpoint::{CheckpointList, CheckpointVerifier, VerifyCheckpointError},
    BoxError, Config,
};

/// The maximum expected gap between blocks.
///
/// Used to identify unexpected out of order blocks.
const MAX_EXPECTED_BLOCK_GAP: u32 = 100_000;

/// The chain verifier routes requests to either the checkpoint verifier or the
/// block verifier, depending on the maximum checkpoint height.
struct ChainVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    block: BlockVerifier<S>,
    checkpoint: CheckpointVerifier<S>,
    max_checkpoint_height: block::Height,
    last_block_height: Option<block::Height>,
}

#[derive(Debug, Display, Error)]
pub enum VerifyChainError {
    /// block could not be checkpointed
    Checkpoint(VerifyCheckpointError),
    /// block could not be verified
    Block(VerifyBlockError),
}

impl<S> Service<Arc<Block>> for ChainVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyChainError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match (self.checkpoint.poll_ready(cx), self.block.poll_ready(cx)) {
            // First, fail if either service fails.
            (Poll::Ready(Err(e)), _) => Poll::Ready(Err(VerifyChainError::Checkpoint(e))),
            (_, Poll::Ready(Err(e))) => Poll::Ready(Err(VerifyChainError::Block(e))),
            // Second, we're unready if either service is unready.
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            // Finally, we're ready if both services are ready and OK.
            (Poll::Ready(Ok(())), Poll::Ready(Ok(()))) => Poll::Ready(Ok(())),
        }
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let height = block.coinbase_height();
        let span = tracing::info_span!("ChainVerifier::call", ?height);
        let _entered = span.enter();
        tracing::debug!("verifying new block");

        // TODO: do we still need this logging?
        // Log an info-level message on unexpected out of order blocks
        let is_unexpected_gap = match (height, self.last_block_height) {
            (Some(block::Height(height)), Some(block::Height(last_block_height)))
                if (height > last_block_height + MAX_EXPECTED_BLOCK_GAP)
                    || (height + MAX_EXPECTED_BLOCK_GAP < last_block_height) =>
            {
                self.last_block_height = Some(block::Height(height));
                true
            }
            (Some(height), _) => {
                self.last_block_height = Some(height);
                false
            }
            // The other cases are covered by the verifiers
            _ => false,
        };
        // Log a message on unexpected out of order blocks.
        //
        // The sync service rejects most of these blocks, but we
        // still want to know if a large number get through.
        if is_unexpected_gap {
            tracing::debug!(
                "large block height gap: this block or the previous block is out of order"
            );
        }

        self.last_block_height = height;

        if let Some(height) = height {
            if height <= self.max_checkpoint_height {
                return self
                    .checkpoint
                    .call(block)
                    .map_err(VerifyChainError::Checkpoint)
                    .boxed();
            }
        }

        // For the purposes of routing requests, we can send blocks
        // with no height to the block verifier, which will reject them.
        //
        // We choose the block verifier because it doesn't have any
        // internal state, so it will always return the same error for a
        // block with no height.
        self.block
            .call(block)
            .map_err(VerifyChainError::Block)
            .boxed()
    }
}

/// Return a block verification service, using `config`, `network` and
/// `state_service`.
///
/// This function should only be called once for a particular state service.
#[instrument(skip(state_service))]
pub async fn init<S>(
    config: Config,
    network: Network,
    mut state_service: S,
) -> Buffer<BoxService<Arc<Block>, block::Hash, VerifyChainError>, Arc<Block>>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let list = CheckpointList::new(network);

    let max_checkpoint_height = if config.checkpoint_sync {
        list.max_height()
    } else {
        list.min_height_in_range(Sapling.activation_height(network).unwrap()..)
            .expect("hardcoded checkpoint list extends past sapling activation")
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

    let block = BlockVerifier::new(network, state_service.clone());
    let checkpoint = CheckpointVerifier::from_checkpoint_list(list, tip, state_service);

    Buffer::new(
        BoxService::new(ChainVerifier {
            block,
            checkpoint,
            max_checkpoint_height,
            last_block_height: None,
        }),
        3,
    )
}
