//! Checkpoint-based block verification for Zebra.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to speed up the
//! initial chain sync for Zebra. This list is distributed with Zebra.
//!
//! The CheckpointVerifier compares each block's `BlockHeaderHash` against the known
//! checkpoint hashes. If it matches, then the block is verified, and added to the
//! `ZebraState`. Otherwise, if the block's height is lower than the maximum checkpoint
//! height, the block awaits the verification of its child block.
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use futures_util::FutureExt;
use std::{
    collections::HashMap,
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

struct CheckpointVerifier<S> {
    /// The underlying `ZebraState`.
    state_service: S,

    /// Each checkpoint consists of a coinbase height and block header hash.
    ///
    /// Checkpoints should be chosen to avoid forks or chain reorganizations,
    /// which only happen in the last few hundred blocks in the chain.
    /// (zcashd allows chain reorganizations up to 99 blocks, and prunes
    /// orphaned side-chains after 288 blocks.)
    checkpoint_list: Option<HashMap<BlockHeight, BlockHeaderHash>>,
}

/// The error type for the CheckpointVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The CheckpointVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<S> Service<Arc<Block>> for CheckpointVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>,
    S::Future: Send + 'static,
{
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.state_service.poll_ready(context)
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report, handle errors from state_service.

        // These checks are cheap, so we can do them in the call()

        let checkpoints = match &self.checkpoint_list {
            Some(checkpoints) => checkpoints,
            None => return async { Err("the checkpoint list is empty".into()) }.boxed(),
        };

        let block_height = match block.coinbase_height() {
            Some(height) => height,
            None => {
                return async { Err("the block does not have a coinbase height".into()) }.boxed()
            }
        };

        // TODO(teor):
        //   - implement chaining from checkpoints to their ancestors
        //   - if chaining is expensive, move this check to the Future
        //   - should the state contain a mapping from previous_block_hash to block?
        let checkpoint_hash_ref = match checkpoints.get(&block_height) {
            Some(hash) => hash,
            None => {
                return async { Err("the block's height is not a checkpoint height".into()) }
                    .boxed()
            }
        };

        // Avoid moving a reference into the future.
        let checkpoint_hash = *checkpoint_hash_ref;

        // `state_service.call` is OK here because we already called
        // `state_service.poll_ready` in our `poll_ready`.
        let add_block = self.state_service.call(zebra_state::Request::AddBlock {
            block: block.clone(),
        });

        async move {
            // Hashing is expensive, so we do it in the Future
            if BlockHeaderHash::from(block.as_ref()) != checkpoint_hash {
                // The block is on a side-chain
                return Err("the block hash does not match the checkpoint hash".into());
            }

            match add_block.await? {
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

// TODO(teor): add a function for the maximum checkpoint height
// We can pre-calculate the result in init(), if we want.

/// Return a checkpoint verification service, using the provided state service.
///
/// The checkpoint verifier holds a state service of type `S`, into which newly
/// verified blocks will be committed. This state is pluggable to allow for
/// testing or instrumentation.
///
/// The returned type is opaque to allow instrumentation or other wrappers, but
/// can be boxed for storage. It is also `Clone` to allow sharing of a
/// verification service.
///
/// This function should be called only once for a particular state service (and
/// the result be shared) rather than constructing multiple verification services
/// backed by the same state layer.
pub fn init<S>(
    state_service: S,
    checkpoint_list: Option<HashMap<BlockHeight, BlockHeaderHash>>,
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
        + 'static,
    S::Future: Send + 'static,
{
    Buffer::new(
        CheckpointVerifier {
            state_service,
            checkpoint_list,
        },
        1,
    )
}

// TODO(teor): tests
