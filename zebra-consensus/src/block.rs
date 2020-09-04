//! Block verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (depends on previous blocks)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

mod check;

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
};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::block::{self, Block};

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
    /// The underlying state service, possibly wrapped in other services.
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
    type Response = block::Hash;
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
        let mut state_service = self.state_service.clone();

        // TODO(jlusby): Error = Report, handle errors from state_service.
        async move {
            let hash = block.hash();

            // These checks only apply to generated blocks. We check the block
            // height for parsed blocks when we deserialize them.
            let height = block
                .coinbase_height()
                .ok_or_else(|| format!("invalid block {:?}: missing block height",
                                       hash))?;
            if height > block::Height::MAX {
                Err(format!("invalid block height {:?} in {:?}: greater than the maximum height {:?}",
                            height,
                            hash,
                            block::Height::MAX))?;
            }

            // Check that this block is actually a new block
            if BlockVerifier::get_block(&mut state_service, hash).await?.is_some() {
                Err(format!("duplicate block {:?} {:?}: block has already been verified",
                            height,
                            hash))?;
            }

            // Do the difficulty checks first, to raise the threshold for
            // attacks that use any other fields.
            let difficulty_threshold = block
                .header
                .difficulty_threshold
                .to_expanded()
                .ok_or_else(|| format!("invalid difficulty threshold in block header {:?} {:?}",
                                       height,
                                       hash))?;
            if hash > difficulty_threshold {
                Err(format!("block {:?} failed the difficulty filter: hash {:?} must be less than or equal to the difficulty threshold {:?}",
                            height,
                            hash,
                            difficulty_threshold))?;
            }
            block.header.is_equihash_solution_valid()?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Field validity and structure checks
            let now = Utc::now();
            block.header.is_time_valid_at(now)?;
            check::is_coinbase_first(&block)?;

            // TODO: context-free header verification: merkle root

            tracing::trace!("verified block");
            metrics::gauge!(
                "block.verified.block.height",
                height.0 as _
            );
            metrics::counter!("block.verified.block.count", 1);

            // Commit the block in the future - the state will handle out of
            // order blocks.
            let ready_state = state_service
                .ready_and()
                .await?;

            match ready_state.call(zebra_state::Request::AddBlock { block }).await? {
                zebra_state::Response::Added { hash: committed_hash } => {
                    assert_eq!(committed_hash, hash, "state returned wrong hash: hashes must be equal");
                    Ok(hash)
                }
                _ => Err(format!("adding block {:?} {:?} to state failed", height, hash))?,
            }
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
    /// Returns an error if `state_service.poll_ready` errors.
    async fn get_block(
        state_service: &mut S,
        hash: block::Hash,
    ) -> Result<Option<Arc<Block>>, Report> {
        let block = state_service
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
}

/// Return a block verification service, using the provided state service.
///
/// The block verifier holds a state service of type `S`, into which newly
/// verified blocks will be committed. This state is pluggable to allow for
/// testing or instrumentation.
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
    Response = block::Hash,
    Error = Error,
    Future = impl Future<Output = Result<block::Hash, Error>>,
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
