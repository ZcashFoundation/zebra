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
use thiserror::Error;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    work::equihash,
};
use zebra_state as zs;

use crate::error::*;
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

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum VerifyBlockError {
    #[error("unable to verify depth for block {hash} from chain state during block verification")]
    Depth { source: BoxError, hash: block::Hash },
    #[error(transparent)]
    Block {
        #[from]
        source: BlockError,
    },
    #[error(transparent)]
    Equihash {
        #[from]
        source: equihash::Error,
    },
    #[error(transparent)]
    Time(BoxError),
    #[error("unable to commit block after semantic verification")]
    Commit(#[source] BoxError),
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
    type Error = VerifyBlockError;
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
            match state_service
                .ready_and()
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
                .call(zs::Request::Depth(hash))
                .await
                .map_err(|source| VerifyBlockError::Depth { source, hash })?
            {
                zs::Response::Depth(Some(depth)) => {
                    return Err(BlockError::AlreadyInChain(hash, depth).into())
                }
                zs::Response::Depth(None) => {}
                _ => unreachable!("wrong response to Request::Depth"),
            }

            // These checks only apply to generated blocks. We check the block
            // height for parsed blocks when we deserialize them.
            let height = block
                .coinbase_height()
                .ok_or(BlockError::MissingHeight(hash))?;
            if height > block::Height::MAX {
                Err(BlockError::MaxHeight(height, hash, block::Height::MAX))?;
            }

            // Do the difficulty checks first, to raise the threshold for
            // attacks that use any other fields.
            let difficulty_threshold = block
                .header
                .difficulty_threshold
                .to_expanded()
                .ok_or(BlockError::InvalidDifficulty(height, hash))?;
            if hash > difficulty_threshold {
                Err(BlockError::DifficultyFilter(
                    height,
                    hash,
                    difficulty_threshold,
                ))?;
            }
            check::is_equihash_solution_valid(&block.header)?;

            // Since errors cause an early exit, try to do the
            // quick checks first.

            // Field validity and structure checks
            let now = Utc::now();
            check::is_time_valid_at(&block.header, now).map_err(VerifyBlockError::Time)?;
            check::is_coinbase_first(&block)?;

            // TODO: context-free header verification: merkle root

            tracing::trace!("verified block");
            metrics::gauge!("block.verified.block.height", height.0 as _);
            metrics::counter!("block.verified.block.count", 1);

            // Finally, submit the block for contextual verification.
            match state_service
                .ready_and()
                .await
                .map_err(VerifyBlockError::Commit)?
                .call(zs::Request::CommitBlock { block })
                .await
                .map_err(VerifyBlockError::Commit)?
            {
                zs::Response::Committed(committed_hash) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    Ok(hash)
                }
                _ => unreachable!("wrong response for CommitBlock"),
            }
        }
        .boxed()
    }
}
