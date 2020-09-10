#[cfg(test)]
mod tests;

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade::Sapling},
};

use zebra_state as zs;

use crate::{
    block::BlockVerifier,
    checkpoint::{CheckpointList, CheckpointVerifier},
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

impl<S> Service<Arc<Block>> for ChainVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We don't expect the verifiers to exert backpressure on our
        // users, so we don't need to call the verifier's `poll_ready` here.
        // (And we don't know which verifier to choose at this point, anyway.)
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let height = block.coinbase_height();

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

        // The only valid block without a coinbase height is the genesis block,
        // which omitted it by mistake.  So for the purposes of routing requests,
        // we can interpret a missing coinbase height as 0; the checkpoint verifier
        // will reject it.
        if height.unwrap_or(block::Height(0)) < self.max_checkpoint_height {
            self.checkpoint.call(block)
        } else {
            self.block.call(block)
        }
    }
}

/// Return a block verification service, using `config`, `network` and
/// `state_service`.
///
/// This function should only be called once for a particular state service.
pub async fn init<S>(
    config: Config,
    network: Network,
    mut state_service: S,
) -> Buffer<BoxService<Arc<Block>, block::Hash, BoxError>, Arc<Block>>
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

    let block = BlockVerifier::new(state_service.clone());
    let checkpoint = CheckpointVerifier::from_checkpoint_list(list, tip, state_service);

    Buffer::new(
        BoxService::new(ChainVerifier {
            block,
            checkpoint,
            max_checkpoint_height,
            last_block_height: None,
        }),
        1,
    )
}
