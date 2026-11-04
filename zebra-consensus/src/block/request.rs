//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::Block;

use super::DecodedBlock;

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation, then asks the state to validate context and commit the block.
    Commit(Arc<Block>),
    /// Validates a body received on a bilaterally negotiated early-relay connection.
    ///
    /// Pre-NU5 blocks and checkpoint verification retain ordinary validation and scoring.
    /// For later blocks, authenticate the header and entire body against a committed parent
    /// before exempting subsequent transaction/contextual failures from peer punishment.
    /// Unavailable parent context defers authentication until a later failure needs peer scoring.
    CommitEarlyRelay(Arc<Block>),
    /// Validates and commits a decoded submission, optionally carrying mined-block notifications
    /// into the state writer.
    ///
    /// Exact decoder matches avoid recomputing proposal identity while their cache entry is live,
    /// and reuse the proposal's semantic and contextual verdicts after proof-of-work, size, and
    /// time checks. All submitted-header and live-state checks still run. Misses use ordinary
    /// verification.
    ///
    /// With notifications, the state checks live parent eligibility before publishing a
    /// validated notification, and publishes committed notifications before replying.
    /// Checkpoint-routed blocks publish only committed notifications.
    CommitDecoded(DecodedBlock, Option<zebra_state::MinedBlockNotifications>),
    /// Performs semantic validation but skips checking proof of work,
    /// then asks the state to perform contextual validation.
    /// Does not commit the block to the state.
    CheckProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        match self {
            Request::Commit(block)
            | Request::CommitEarlyRelay(block)
            | Request::CheckProposal(block) => Arc::clone(block),
            Request::CommitDecoded(decoded, _) => decoded.block(),
        }
    }

    /// Returns `true` if the request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(_) | Request::CommitEarlyRelay(_) | Request::CommitDecoded(_, _) => {
                false
            }
            Request::CheckProposal(_) => true,
        }
    }
}
