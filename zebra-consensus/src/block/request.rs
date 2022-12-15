//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::Block;

#[derive(Debug, Clone, PartialEq, Eq)]
/// A request to the chain or block verifier
pub enum Request {
    /// Performs semantic validation then calls state with CommitBlock request
    Commit(Arc<Block>),

    #[cfg(feature = "getblocktemplate-rpcs")]
    /// Performs semantic validation but skips checking the solution,
    /// then calls the state with CheckBlockValid request
    CheckProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Commit(block) => block,

            #[cfg(feature = "getblocktemplate-rpcs")]
            Request::CheckProposal(block) => block,
        })
    }

    /// Checks if request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Commit(_) => false,

            #[cfg(feature = "getblocktemplate-rpcs")]
            Request::CheckProposal(_) => true,
        }
    }
}
