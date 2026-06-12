//! Checkpoint heights and the mandatory-checkpoint commit gate.
//!
//! The checkpoint *verifier* was replaced by the known-hash IBD engine
//! (`docs/design/known-hash-ibd.md`), which commits checkpoint-range blocks
//! against a pinned hash list through the state's checkpoint path. What
//! remains in this crate is stateless: the network's checkpoint heights, and
//! [`CheckpointGateLayer`], which rejects semantic commits at or below the
//! network's mandatory checkpoint height before they reach the verifier and
//! the state service.
//!
//! Zebra cannot fully validate blocks at or below the mandatory checkpoint
//! height (Canopy activation): some consensus rules are not implemented for
//! those network upgrades, so the only sound way to commit them is
//! checkpoint-equivalent verification. The semantic block verifier processes
//! any block above the mandatory height; callers must route
//! checkpoint-verifiable blocks through the known-hash engine, and must not
//! send blocks in the engine's range here unless checkpoint sync is disabled
//! in the config.

use std::task::{Context, Poll};

use futures::future::{self, Either, Ready};
use tower::{Layer, Service};

use zebra_chain::{block, parameters::Network};

use crate::{
    block::{Request, VerifyBlockError},
    Config,
};

/// Returns the maximum checkpoint verification height for `network` and
/// `config`.
///
/// This is the end of the network's checkpoint list when checkpoint sync is
/// enabled, and the first checkpoint at or above the mandatory checkpoint
/// height when it is disabled (users who want full validation from Canopy
/// onwards can disable `checkpoint_sync`).
pub fn max_checkpoint_height(config: Config, network: &Network) -> block::Height {
    // TODO: Zebra parses the checkpoint list multiple times at startup.
    //       Instead, cache the checkpoint list for each `network`.
    let list = network.checkpoint_list();

    if config.checkpoint_sync {
        list.max_height()
    } else {
        list.min_height_in_range(network.mandatory_checkpoint_height()..)
            .expect("hardcoded checkpoint list extends past canopy activation")
    }
}

/// A [`Layer`] that gates semantic block commits at the network's mandatory
/// checkpoint height.
///
/// Wraps the semantic block verifier, rejecting any block at or below the
/// mandatory checkpoint height with
/// [`VerifyBlockError::BelowMandatoryCheckpoint`] before the verifier or the
/// state service see it (see the [module docs](self)).
#[derive(Clone, Debug)]
pub struct CheckpointGateLayer {
    /// The network's mandatory checkpoint height.
    floor: block::Height,
}

impl CheckpointGateLayer {
    /// Returns a layer that gates commits at or below `network`'s mandatory
    /// checkpoint height.
    pub fn new(network: &Network) -> Self {
        Self {
            floor: network.mandatory_checkpoint_height(),
        }
    }
}

impl<S> Layer<S> for CheckpointGateLayer {
    type Service = CheckpointGate<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CheckpointGate {
            floor: self.floor,
            inner,
        }
    }
}

/// A stateless gate in front of the semantic block verifier: rejects commit
/// and proposal requests at or below the mandatory checkpoint height, and
/// passes everything else through unchanged.
///
/// Built by [`CheckpointGateLayer`]; see the [module docs](self) for why the
/// floor exists.
#[derive(Clone, Debug)]
pub struct CheckpointGate<S> {
    /// The network's mandatory checkpoint height.
    floor: block::Height,

    /// The wrapped semantic block verifier.
    inner: S,
}

impl<S> Service<Request> for CheckpointGate<S>
where
    S: Service<Request, Response = block::Hash, Error = VerifyBlockError>,
{
    type Response = block::Hash;
    type Error = VerifyBlockError;
    type Future = Either<Ready<Result<block::Hash, VerifyBlockError>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // Blocks without a parseable coinbase height pass through, so the
        // verifier reports them as `MissingHeight` with full context.
        let height = request.block().coinbase_height();

        if let Some(height) = height {
            if height <= self.floor {
                // There's no use case for block proposals at or below the
                // mandatory checkpoint.
                let error = if request.is_proposal() {
                    VerifyBlockError::ValidateProposal(
                        "block proposals must be above the mandatory checkpoint height".into(),
                    )
                } else {
                    VerifyBlockError::BelowMandatoryCheckpoint {
                        height,
                        floor: self.floor,
                    }
                };

                return Either::Left(future::ready(Err(error)));
            }
        }

        Either::Right(self.inner.call(request))
    }
}
