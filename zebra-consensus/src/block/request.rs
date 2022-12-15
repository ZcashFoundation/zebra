//! Block verifier request type.

use std::sync::Arc;

use zebra_chain::block::Block;

pub enum Request {
    Block(Arc<Block>),

    #[cfg(feature = "getblocktemplate-rpcs")]
    BlockProposal(Arc<Block>),
}

impl Request {
    /// Returns inner block
    pub fn block(&self) -> Arc<Block> {
        Arc::clone(match self {
            Request::Block(block) => block,

            #[cfg(feature = "getblocktemplate-rpcs")]
            Request::BlockProposal(block) => block,
        })
    }

    /// Checks if request is a proposal
    pub fn is_proposal(&self) -> bool {
        match self {
            Request::Block(_) => false,

            #[cfg(feature = "getblocktemplate-rpcs")]
            Request::BlockProposal(_) => true,
        }
    }
}
