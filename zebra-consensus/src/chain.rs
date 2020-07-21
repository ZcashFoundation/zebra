//! Chain state updates for Zebra.
//!
//! Chain state updates occur in multiple stages:
//!   - verify blocks (using `BlockVerifier` or `CheckpointVerifier`)
//!   - update the list of verified blocks on disk
//!   - create the chain state needed to verify child blocks
//!   - choose the best tip from all the available chain tips
//!   - update the mature chain state on disk
//!   - prune orphaned side-chains
//!
//! Chain state updates are provided via a `tower::Service`, to support
//! backpressure and batch verification.

#[cfg(test)]
mod tests;

use crate::checkpoint::CheckpointVerifier;

use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;
use zebra_chain::Network;

struct ChainVerifier<BV, S> {
    /// The underlying `BlockVerifier`, possibly wrapped in other services.
    block_verifier: BV,

    /// The underlying `CheckpointVerifier`, wrapped in a buffer, so we can
    /// clone and share it with futures.
    checkpoint_verifier: Buffer<CheckpointVerifier, Arc<Block>>,
    /// The maximum checkpoint height for `checkpoint_verifier`.
    max_checkpoint_height: BlockHeight,

    /// The underlying `ZebraState`, possibly wrapped in other services.
    state_service: S,
}

/// The error type for the ChainVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The ChainVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<BV, S> Service<Arc<Block>> for ChainVerifier<BV, S>
where
    BV: Service<Arc<Block>, Response = BlockHeaderHash, Error = Error> + Send + Clone + 'static,
    BV::Future: Send + 'static,
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
        // We don't expect the state or verifiers to exert backpressure on our
        // users, so we don't need to call `state_service.poll_ready()` here.
        // (And we don't know which verifier to choose at this point, anyway.)
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report, handle errors from state_service.
        let mut block_verifier = self.block_verifier.clone();
        let mut checkpoint_verifier = self.checkpoint_verifier.clone();
        let mut state_service = self.state_service.clone();
        let max_checkpoint_height = self.max_checkpoint_height;

        async move {
            // Call a verifier based on the block height and checkpoints
            //
            // TODO(teor): for post-sapling checkpoint blocks, allow callers
            //             to use BlockVerifier, CheckpointVerifier, or both.
            match block.coinbase_height() {
                Some(height) if (height <= max_checkpoint_height) => {
                    checkpoint_verifier
                        .ready_and()
                        .await?
                        .call(block.clone())
                        .await?
                }
                Some(_) => {
                    block_verifier
                        .ready_and()
                        .await?
                        .call(block.clone())
                        .await?
                }
                None => return Err("Invalid block: must have a coinbase height".into()),
            };

            // TODO(teor):
            //   - handle chain reorgs
            //   - adjust state_service "unique block height" conditions

            // `Tower::Buffer` requires a 1:1 relationship between `poll()`s
            // and `call()`s, because it reserves a buffer slot in each
            // `call()`.
            let add_block = state_service
                .ready_and()
                .await?
                .call(zebra_state::Request::AddBlock { block });

            match add_block.await? {
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

/// Return a chain verification service, using `network` and the provided state
/// service. The network is used to create a block verifier and checkpoint
/// verifier.
///
/// This function should only be called once for a particular state service. If
/// you need shared block or checkpoint verfiers, create them yourself, and pass
/// them to `init_from_verifiers`.
//
// TODO: revise this interface when we generate our own blocks, or validate
//       mempool transactions. We might want to share the BlockVerifier, and we
//       might not want to add generated blocks to the state.
pub fn init<S>(
    network: Network,
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
    let block_verifier = crate::block::init(state_service.clone());
    let checkpoint_verifier = CheckpointVerifier::new(network);

    init_from_verifiers(block_verifier, checkpoint_verifier, state_service)
}

/// Return a chain verification service, using the provided verifier and state
/// services.
///
/// The chain verifier holds a state service of type `S`, used as context for
/// block validation and to which newly verified blocks will be committed. This
/// state is pluggable to allow for testing or instrumentation.
///
/// The returned type is opaque to allow instrumentation or other wrappers, but
/// can be boxed for storage. It is also `Clone` to allow sharing of a
/// verification service.
///
/// This function should only be called once for a particular state service and
/// verifiers (and the result be shared, cloning if needed). Constructing
/// multiple services from the same underlying state might cause synchronisation
/// bugs.
pub fn init_from_verifiers<BV, S>(
    block_verifier: BV,
    // We use an explcit type, so callers can't accidentally swap the verifiers
    checkpoint_verifier: CheckpointVerifier,
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
    BV: Service<Arc<Block>, Response = BlockHeaderHash, Error = Error> + Send + Clone + 'static,
    BV::Future: Send + 'static,
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    let max_checkpoint_height = checkpoint_verifier.list().max_height();
    // Wrap the checkpoint verifier in a buffer, so we can share it
    let checkpoint_verifier = Buffer::new(checkpoint_verifier, 1);

    Buffer::new(
        ChainVerifier {
            block_verifier,
            checkpoint_verifier,
            max_checkpoint_height,
            state_service,
        },
        1,
    )
}
