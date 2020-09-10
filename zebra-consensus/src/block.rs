//! Block verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (depends on previous blocks)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::Utc;
use futures_util::FutureExt;
use tower::{Service, ServiceExt};

use zebra_chain::block::{self, Block};
use zebra_state as zs;

use crate::BoxError;

mod check;
#[cfg(test)]
mod tests;

/// A service that verifies blocks.
#[derive(Debug)]
pub struct BlockVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    /// The underlying state service, possibly wrapped in other services.
    state_service: S,
}

impl<S> BlockVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    pub fn new(state_service: S) -> Self {
        Self { state_service }
    }
}

impl<S> Service<Arc<Block>> for BlockVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = BoxError;
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

            // Check that this block is actually a new block.
            match state_service.ready_and().await?.call(zs::Request::Depth(hash)).await? {
                zs::Response::Depth(Some(depth)) => {
                    return Err(format!(
                        "block {} is already in the chain at depth {:?}",
                        hash,
                        depth,
                    ).into())
                },
                zs::Response::Depth(None) => {},
                _ => unreachable!("wrong response to Request::Depth"),
            }

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
            check::is_equihash_solution_valid(&block.header)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Field validity and structure checks
            let now = Utc::now();
            check::is_time_valid_at(&block.header, now)?;
            check::is_coinbase_first(&block)?;

            // TODO: context-free header verification: merkle root

            tracing::trace!("verified block");
            metrics::gauge!(
                "block.verified.block.height",
                height.0 as _
            );
            metrics::counter!("block.verified.block.count", 1);

            // Finally, submit the block for contextual verification.
            match state_service.oneshot(zs::Request::CommitBlock{ block }).await? {
                zs::Response::Committed(committed_hash) => {
                    assert_eq!(committed_hash, hash, "state returned wrong hash: hashes must be equal");
                    Ok(hash)
                }
                _ => unreachable!("wrong response to CommitBlock"),
            }
        }
        .boxed()
    }
}
