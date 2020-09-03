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

use crate::checkpoint::{CheckpointList, CheckpointVerifier};
use crate::Config;

use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::{self, Block};
use zebra_chain::parameters::{Network, NetworkUpgrade::Sapling};

/// The maximum expected gap between blocks.
///
/// Used to identify unexpected out of order blocks.
const MAX_EXPECTED_BLOCK_GAP: u32 = 100_000;

/// A wrapper type that holds the `ChainVerifier`'s `CheckpointVerifier`, and
/// its associated state.
#[derive(Clone)]
struct ChainCheckpointVerifier {
    /// The underlying `CheckpointVerifier`, wrapped in a buffer, so we can
    /// clone and share it with futures.
    verifier: Buffer<CheckpointVerifier, Arc<Block>>,

    /// The maximum checkpoint height for `checkpoint_verifier`.
    max_height: block::Height,
}

/// A service that verifies the chain, using its associated `CheckpointVerifier`
/// and `BlockVerifier`.
struct ChainVerifier<BV, S>
where
    BV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    BV::Future: Send + 'static,
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    /// The underlying `BlockVerifier`, possibly wrapped in other services.
    block_verifier: BV,

    /// The `ChainVerifier`'s underlying `CheckpointVerifier`, and its
    /// associated state.
    ///
    /// None if all the checkpoints have been verified.
    checkpoint: Option<ChainCheckpointVerifier>,

    /// The underlying `ZebraState`, possibly wrapped in other services.
    state_service: S,

    /// The most recent block height that was submitted to the verifier.
    ///
    /// Used for debugging.
    ///
    /// Updated before verification: the block at this height might not be valid.
    last_block_height: Option<block::Height>,
}

/// The error type for the ChainVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The ChainVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<BV, S> Service<Arc<Block>> for ChainVerifier<BV, S>
where
    BV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    BV::Future: Send + 'static,
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
        // We don't expect the state or verifiers to exert backpressure on our
        // users, so we don't need to call `state_service.poll_ready()` here.
        // (And we don't know which verifier to choose at this point, anyway.)
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report, handle errors from state_service.
        let height = block.coinbase_height();
        let hash = block.hash();
        let span = tracing::debug_span!("block_verify", ?height, ?hash,);

        let mut block_verifier = self.block_verifier.clone();
        let mut state_service = self.state_service.clone();
        let checkpoint_verifier = self.checkpoint.clone().map(|c| c.verifier);
        let max_checkpoint_height = self.checkpoint.clone().map(|c| c.max_height);

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

        async move {
            // Call a verifier based on the block height and checkpoints.
            if is_higher_than_max_checkpoint(height, max_checkpoint_height) {
                // Log a message on unexpected out of order blocks.
                //
                // The sync service rejects most of these blocks, but we
                // still want to know if a large number get through.
                if is_unexpected_gap {
                    tracing::debug!("large block height gap: this block or the previous block is out of order");
                }

                block_verifier
                    .ready_and()
                    .await?
                    .call(block.clone())
                    .await?;
            } else {
                checkpoint_verifier
                    .expect("missing checkpoint verifier: verifier must be Some if max checkpoint height is Some")
                    .ready_and()
                    .await?
                    .call(block.clone())
                    .await?;
            }

            tracing::trace!(?height, ?hash, "verified block");
            metrics::gauge!(
                "chain.verified.block.height",
                height.expect("valid blocks have a block height").0 as _
            );
            metrics::counter!("chain.verified.block.count", 1);

            let add_block = state_service
                .ready_and()
                .await?
                .call(zebra_state::Request::AddBlock { block });

            match add_block.await? {
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .instrument(span)
        .boxed()
    }
}

/// Returns true if block_height is higher than the maximum checkpoint
/// height. Also returns true if there is no maximum checkpoint height.
///
/// Returns false if the block does not have a height.
fn is_higher_than_max_checkpoint(
    block_height: Option<block::Height>,
    max_checkpoint_height: Option<block::Height>,
) -> bool {
    match (block_height, max_checkpoint_height) {
        (Some(block_height), Some(max_checkpoint_height)) => block_height > max_checkpoint_height,
        (_, None) => true,
        (None, _) => false,
    }
}

/// Return a chain verification service, using `config`, `network` and
/// `state_service`.
///
/// Gets the initial tip from the state service, and uses it to create a block
/// verifier and checkpoint verifier (if needed).
///
/// This function should only be called once for a particular state service. If
/// you need shared block or checkpoint verfiers, create them yourself, and pass
/// them to `init_from_verifiers`.
//
// TODO: revise this interface when we generate our own blocks, or validate
//       mempool transactions. We might want to share the BlockVerifier, and we
//       might not want to add generated blocks to the state.
pub async fn init<S>(
    config: Config,
    network: Network,
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
    let initial_tip = zebra_state::current_tip(state_service.clone())
        .await
        .expect("State service poll_ready is Ok");

    let block_verifier = crate::block::init(state_service.clone());
    let checkpoint_list = match config.checkpoint_sync {
        true => CheckpointList::new(network),
        false => CheckpointList::new_up_to(network, Sapling),
    };

    init_from_verifiers(
        network,
        block_verifier,
        Some(checkpoint_list),
        state_service,
        initial_tip,
    )
}

/// Return a chain verification service, using the provided block verifier,
/// checkpoint list, and state service.
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
/// block verifier (and the result be shared, cloning if needed). Constructing
/// multiple services from the same underlying state might cause synchronisation
/// bugs.
///
/// Panics:
///
/// Panics if the `checkpoint_list` is None, and the `initial_tip_height` is
/// below the Sapling network upgrade for `network`. (The `block_verifier` can't
/// verify all the constraints on pre-Sapling blocks, so they require
/// checkpoints.)
pub(crate) fn init_from_verifiers<BV, S>(
    network: Network,
    block_verifier: BV,
    checkpoint_list: Option<CheckpointList>,
    state_service: S,
    initial_tip: Option<Arc<Block>>,
) -> impl Service<
    Arc<Block>,
    Response = block::Hash,
    Error = Error,
    Future = impl Future<Output = Result<block::Hash, Error>>,
> + Send
       + Clone
       + 'static
where
    BV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    BV::Future: Send + 'static,
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    let max_checkpoint_height = checkpoint_list.clone().map(|c| c.max_height());
    let initial_height = initial_tip.clone().map(|b| b.coinbase_height()).flatten();
    let initial_hash = initial_tip.clone().map(|b| b.hash());

    tracing::info!(
        ?network,
        ?max_checkpoint_height,
        ?initial_height,
        ?initial_hash,
        "initialising ChainVerifier"
    );

    let sapling_activation = Sapling
        .activation_height(network)
        .expect("Unexpected network upgrade info: Sapling must have an activation height");

    let checkpoint = match (initial_height, checkpoint_list, max_checkpoint_height) {
        // If we need to verify pre-Sapling blocks, make sure we have checkpoints for them.
        (None, None, _) => panic!("We have no checkpoints, and we have no cached blocks: Pre-Sapling blocks must be verified by checkpoints"),
        (Some(initial_height), None, _) if (initial_height < sapling_activation) => panic!("We have no checkpoints, and we don't have a cached Sapling activation block: Pre-Sapling blocks must be verified by checkpoints"),

        // If we're past the checkpoint range, don't create a checkpoint verifier.
        (Some(initial_height), _, Some(max_checkpoint_height)) if (initial_height > max_checkpoint_height) => None,
        // No list, no checkpoint verifier
        (_, None, _) => None,

        (_, Some(_), None) => panic!("Missing max checkpoint height: height must be Some if verifier is Some"),
        // We've done all the checks we need to create a checkpoint verifier
        (_, Some(list), Some(max_height)) => Some(
            ChainCheckpointVerifier {
                verifier: Buffer::new(CheckpointVerifier::from_checkpoint_list(list, initial_tip), 1),
                max_height,
            }),
    };

    Buffer::new(
        ChainVerifier {
            block_verifier,
            checkpoint,
            state_service,
            last_block_height: initial_height,
        },
        1,
    )
}
