//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::Block;

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation, then asks the state to perform contextual validation and commit the block
    Commit(Arc<Block>),

    /// Submit a solved block with an optional-template work identifier used only as a cache hint.
    /// A wrong hint falls back to normal validation; actual bytes and proof of work are checked.
    CommitWithWorkId {
        /// The solved block to validate and commit.
        block: Arc<Block>,
        /// The normalized proposal key supplied by the miner.
        work_id: [u8; 32],
    },
    /// Performs semantic validation but skips checking proof of work,
    /// then asks the state to perform contextual validation.
    /// Does not commit the block to the state.
    CheckProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Commit(block) => block,
            Request::CommitWithWorkId { block, .. } => block,
            Request::CheckProposal(block) => block,
        })
    }

    /// Returns `true` if the request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(_) | Request::CommitWithWorkId { .. } => false,
            Request::CheckProposal(_) => true,
        }
    }

    /// Return the optional cache lookup hint; it never authorizes validation by itself.
    pub fn work_id(&self) -> Option<[u8; 32]> {
        match self {
            Request::CommitWithWorkId { work_id, .. } => Some(*work_id),
            _ => None,
        }
    }
}
