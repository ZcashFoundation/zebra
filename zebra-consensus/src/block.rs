//! Block verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (depends on previous blocks)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

#[cfg(test)]
mod tests;

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time;
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

/// A service that verifies blocks.
#[derive(Debug)]
struct BlockVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    /// The underlying `ZebraState`, possibly wrapped in other services.
    // TODO: contextual verification
    #[allow(dead_code)]
    state_service: S,
}

/// The error type for the BlockVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The BlockVerifier service implementation.
///
/// The state service is only used for contextual verification.
/// (The `ChainVerifier` updates the state.)
impl<S> Service<Arc<Block>> for BlockVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We use the state for contextual verification, and we expect those
        // queries to be fast. So we don't need to call
        // `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let mut state = self.state_service.clone();

        // TODO(jlusby): Error = Report, handle errors from state_service.
        async move {
            // Since errors cause an early exit, try to do the
            // quick checks first.

            let now = Utc::now();
            block.header.is_time_valid_at(now)?;
            block.header.is_equihash_solution_valid()?;
            block.is_coinbase_first()?;

            // These checks only apply to generated blocks. We check the block
            // height for parsed blocks when we deserialize them.
            let height = block
                .coinbase_height()
                .ok_or("Invalid block: missing block height")?;
            if height > BlockHeight::MAX {
                Err("Invalid block height: greater than the maximum height.")?;
            }

            // As a temporary solution for chain gaps, wait for the previous block,
            // and check its height.
            // TODO:
            //   - Add a previous block height and hash constraint to the AddBlock request,
            //     so that we can verify in parallel, then check constraints before committing
            //
            // Skip contextual checks for the genesis block
            let previous_block_hash = block.header.previous_block_hash;
            if previous_block_hash != crate::parameters::GENESIS_PREVIOUS_BLOCK_HASH {
                tracing::debug!(?height, "Awaiting previous block from state");
                let previous_block = BlockVerifier::await_block(
                    &mut state,
                    previous_block_hash,
                    BlockHeight(height.0 - 1),
                )
                .await?;

                let previous_height = previous_block.coinbase_height().unwrap();
                if height.0 != previous_height.0 + 1 {
                    Err("Invalid block height: must be 1 more than the previous block height.")?;
                }
            }

            // TODO:
            //   - header verification
            //   - contextual verification

            Ok(block.as_ref().into())
        }
        .boxed()
    }
}

/// The BlockVerifier implementation.
///
/// The state service is only used for contextual verification.
/// (The `ChainVerifier` updates the state.)
impl<S> BlockVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    /// Get the block for `hash`, using `state`.
    ///
    /// If there is no block for that hash, returns `Ok(None)`.
    /// Returns an error if `state.poll_ready` errors.
    async fn get_block(state: &mut S, hash: BlockHeaderHash) -> Result<Option<Arc<Block>>, Report> {
        let block = state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock { hash })
            .await
            .map(|response| match response {
                zebra_state::Response::Block { block } => block,
                _ => unreachable!("GetBlock request can only result in Response::Block"),
            })
            .ok();

        Ok(block)
    }

    /// Wait until a block with `hash` is in `state`.
    ///
    /// Returns an error if `state.poll_ready` errors.
    async fn await_block(
        state: &mut S,
        hash: BlockHeaderHash,
        height: BlockHeight,
    ) -> Result<Arc<Block>, Report> {
        loop {
            match BlockVerifier::get_block(state, hash).await? {
                Some(block) => return Ok(block),
                // Busy-waiting is only a temporary solution to waiting for blocks.
                // TODO:
                //   - Get an AwaitBlock future from the state
                //   - Replace with AddBlock constraints
                None => {
                    tracing::debug!(?height, ?hash, "Waiting for state to have block");
                    time::delay_for(Duration::from_secs(2)).await
                }
            };
        }
    }
}

/// Return a block verification service, using the provided state service.
///
/// The block verifier holds a state service of type `S`, used as context for
/// block validation. This state is pluggable to allow for testing or
/// instrumentation.
///
/// The returned type is opaque to allow instrumentation or other wrappers, but
/// can be boxed for storage. It is also `Clone` to allow sharing of a
/// verification service.
///
/// This function should be called only once for a particular state service (and
/// the result be shared, cloning if needed). Constructing multiple services
/// from the same underlying state might cause synchronisation bugs.
pub fn init<S>(
    state_service: S,
) -> impl Service<
    Arc<Block>,
    Response = BlockHeaderHash,
    Error = Error,
    Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
> + Send
       + Clone
       + 'static
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    Buffer::new(BlockVerifier { state_service }, 1)
}
