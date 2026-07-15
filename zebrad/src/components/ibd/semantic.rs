//! The full-validation [`CommitStage`] implementation for the IBD engine.
//!
//! Where [`VerifyAndCommit`](super::convert::VerifyAndCommit) is the known-hash
//! checkpoint stage-2 (a merkle-root pin plus
//! [`CommitCheckpointVerifiedBlock`]), [`SemanticCommit`] is the
//! full-validation stage-2: it hands each fetched block to the zebra-consensus
//! block verifier through [`zebra_consensus::Request::Commit`], which runs
//! semantic and contextual validation and commits the block to the
//! non-finalized state. This is the same call the legacy syncer's
//! `download_and_verify` makes per block.
//!
//! Both types implement [`CommitStage`], so the engine drives either one with
//! its window, weighted-fetch, gap-hedge, and commit-pipeline machinery
//! unchanged — the payoff of the [`CommitStage`] seam (design doc §17 / the
//! generic engine unification). `ChainSync` wires this stage together with a
//! [`DiscoverySource`](super::discovery::DiscoverySource) through
//! [`Engine::new_semantic`](super::engine::Engine::new_semantic), replacing
//! the legacy syncer's bespoke `downloads.rs` Hedge/Retry/Timeout stack.
//!
//! [`CommitStage`]: super::convert::CommitStage
//! [`CommitCheckpointVerifiedBlock`]: zebra_state::Request::CommitCheckpointVerifiedBlock

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::FutureExt;
use tower::{Service, ServiceExt};

use zebra_chain::block;
use zebra_consensus::spawn_fifo;

use super::convert::{ConvertError, IbdBlock, VerifyAndCommitError};
use crate::BoxError;

#[cfg(test)]
mod tests;

/// The full-validation stage-2 service: the semantic block verifier behind the
/// engine's [`CommitStage`] seam.
///
/// One call per block: resolve the payload (deserializing raw cached bytes on
/// the rayon pool, re-checking their hash against the assigned `expected`),
/// then `Commit` it through the verifier `ZV`. The verifier performs full
/// semantic and contextual validation and commits to the non-finalized state;
/// the returned future resolves with the committed hash only afterwards, so
/// unresolved futures are the engine's commit-pipeline cap unit exactly as for
/// the known-hash path.
///
/// `ZV` is the same verifier service the legacy syncer holds:
/// `Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>`.
///
/// [`CommitStage`]: super::convert::CommitStage
#[derive(Clone, Debug)]
pub struct SemanticCommit<ZV>
where
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
{
    /// The zebra-consensus block verifier (semantic + contextual + commit).
    verifier: ZV,
}

impl<ZV> SemanticCommit<ZV>
where
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
{
    /// Returns a new full-validation stage-2 over the consensus `verifier`.
    pub fn new(verifier: ZV) -> Self {
        Self { verifier }
    }
}

impl<ZV> Service<IbdBlock> for SemanticCommit<ZV>
where
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
{
    type Response = block::Hash;
    type Error = VerifyAndCommitError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Delegate to the verifier; the engine's real backpressure is its
        // unresolved-future caps, not readiness (design doc §4.3). A verifier
        // that fails readiness is gone, so the engine treats it as a shutdown.
        self.verifier
            .poll_ready(cx)
            .map_err(VerifyAndCommitError::StageUnready)
    }

    fn call(&mut self, request: IbdBlock) -> Self::Future {
        let IbdBlock {
            height,
            expected,
            block,
            ..
        } = request;

        // Clone before the async block (tower convention): the future must not
        // borrow from `self`. The clone's own readiness is established by
        // `oneshot` below.
        let verifier = self.verifier.clone();

        async move {
            // Payload resolution (deserializing raw cached bytes, re-checking
            // their hash against `expected`) runs on the rayon pool, so the
            // engine task never deserializes inline (design doc §4.5). A
            // corrupt cache entry is a verify-stage failure: the engine
            // discards it and refetches, exactly as on the known-hash path.
            let block = spawn_fifo(move || block.into_block(height, expected))
                .await
                .map_err(|_recv_error| VerifyAndCommitError::Verify(ConvertError::RayonShutdown))?
                .map_err(VerifyAndCommitError::Verify)?;

            // Full semantic + contextual validation and the non-finalized
            // commit. The verifier owns reorg handling for its own state, so
            // a block that loses a race is reorged there, not here.
            let committed_hash = match verifier
                .oneshot(zebra_consensus::Request::Commit(block))
                .await
            {
                Ok(committed_hash) => {
                    // The verifier commits the exact block it was handed and
                    // returns that block's hash, which the fetch layer already
                    // matched against the assigned `expected` at receipt.
                    // Assert the parity, matching the known-hash path
                    // (`VerifyAndCommit`): the engine keys the committed slot
                    // by the assigned height, so a divergence here would mark
                    // the wrong block committed.
                    assert_eq!(
                        committed_hash, expected,
                        "the verifier must commit the hash the engine assigned to this height",
                    );

                    committed_hash
                }

                // A duplicate-request error means this exact block is already
                // verified and committed — by a previous sync run, the inbound
                // gossip downloader, or the known-hash engine's overlap. The
                // goal state for this height is reached, so report success:
                // routing it through the engine's frontier commit-reset path
                // would refetch a block the state already has until the
                // byte-identical failure limit gave up. This is the same
                // classification the legacy syncer applied
                // (`VerifyBlockError::is_duplicate_request`).
                //
                // TODO(known-hash-ibd D6): also classify peer-attributable
                // invalid blocks, reporting the implicated copy's source peer
                // through the address book updater.
                Err(error) => match error.downcast::<zebra_consensus::VerifyBlockError>() {
                    Ok(verify_error) if verify_error.is_duplicate_request() => {
                        debug!(
                            height = height.0,
                            hash = %expected,
                            "block was already verified and committed, continuing",
                        );

                        expected
                    }
                    Ok(verify_error) => {
                        return Err(VerifyAndCommitError::Commit {
                            height,
                            hash: expected,
                            error: verify_error,
                        })
                    }
                    Err(error) => {
                        return Err(VerifyAndCommitError::Commit {
                            height,
                            hash: expected,
                            error,
                        })
                    }
                },
            };

            Ok(committed_hash)
        }
        .boxed()
    }
}
