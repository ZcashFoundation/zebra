#[cfg(test)]
mod tests;

use displaydoc::Display;
use futures::FutureExt;
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

/// The chain verifier routes requests to either the checkpoint verifier or the
/// block verifier, depending on the maximum checkpoint height.
struct ChainVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    block: BlockVerifier<S>,
    checkpoint: Buffer<CheckpointVerifier<S>, Arc<block::Block>>,
    max_checkpoint_height: block::Height,
}

#[derive(Debug, Display, Error)]
pub enum VerifyChainError {
    /// block could not be checkpointed
    Checkpoint(#[source] VerifyCheckpointError),
    /// block could not be verified
    Block(#[source] VerifyBlockError),
}

impl<S> Service<Arc<Block>> for ChainVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyChainError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Correctness:
        //
        // We can't call `poll_ready` on the block and checkpoint verifiers here,
        // because each `poll_ready` must be followed by a `call`, and we don't
        // know which verifier we're going to choose yet.
        // See #1593 for details.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let mut b = self.block.clone();
        let mut cp = self.checkpoint.clone();
        let max_checkpoint_height = self.max_checkpoint_height;
        async move {
            match block.coinbase_height() {
                Some(height) if height <= max_checkpoint_height => cp
                    .ready_and()
                    .await
                    .unwrap() // safe because poll_ready is always ok?
                    .call(block)
                    .await
                    .map_err(VerifyChainError::Checkpoint)
                    ,

                // This also covers blocks with no height, which the block verifier
                // will reject immediately.
                _ => b
                    .ready_and()
                    .await
                    .unwrap() // safe because poll_ready is always ok?
                    .call(block)
                    .await
                    .map_err(VerifyChainError::Block)
                    ,
            }
        }
        .boxed()
    }
}

/// Initialize a block verification service.
///
/// The consensus configuration is specified by `config`, and the Zcash network
/// to verify blocks for is specified by `network`.
///
/// The block verification service asynchronously performs semantic verification
/// checks. Blocks that pass semantic verification are submitted to the supplied
/// `state_service` for contextual verification before being committed to the chain.
///
/// This function should only be called once for a particular state service.
///
/// Dropped requests are cancelled on a best-effort basis, but may continue to be processed.
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
    let checkpoint = Buffer::new(
        CheckpointVerifier::from_checkpoint_list(list, tip, state_service),
        3,
    );

    Buffer::new(
        BoxService::new(ChainVerifier {
            block,
            checkpoint,
            max_checkpoint_height,
        }),
        3,
    )
}
