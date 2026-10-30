//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::Block;

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation, then asks the state to perform contextual validation and commit the block
    Commit(Arc<Block>),
    /// Validates and commits a locally mined block, carrying notifications into the state writer.
    ///
    /// Exact successful proposals can reuse their semantic and contextual verdicts after
    /// proof-of-work, size, and time checks. The state checks live parent eligibility before
    /// publishing a validated notification, and publishes committed notifications before replying.
    /// Checkpoint-routed blocks publish only committed notifications.
    CommitMined(Arc<Block>, zebra_state::MinedBlockNotifications),
    /// Performs semantic validation but skips checking proof of work,
    /// then asks the state to perform contextual validation.
    /// Does not commit the block to the state.
    CheckProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Commit(block) | Request::CommitMined(block, _) => block,
            Request::CheckProposal(block) => block,
        })
    }

    /// Returns `true` if the request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(_) | Request::CommitMined(_, _) => false,
            Request::CheckProposal(_) => true,
        }
    }
}
