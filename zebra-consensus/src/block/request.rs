//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::{self, Block};

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation, then asks the state to perform contextual validation and commit the block.
    ///
    /// Carries a pre-computed block hash to avoid redundant hashing.
    Commit(Arc<Block>, block::Hash),
    /// Performs semantic validation but skips checking proof of work,
    /// then asks the state to perform contextual validation.
    /// Does not commit the block to the state.
    CheckProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Commit(block, _) => block,
            Request::CheckProposal(block) => block,
        })
    }

    /// Returns the pre-computed block hash for `Commit` requests, or
    /// computes it on-demand for `CheckProposal` requests.
    pub fn hash(&self) -> block::Hash {
        match self {
            Request::Commit(_, hash) => *hash,
            Request::CheckProposal(block) => block.hash(),
        }
    }

    /// Returns `true` if the request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(..) => false,
            Request::CheckProposal(_) => true,
        }
    }
}
