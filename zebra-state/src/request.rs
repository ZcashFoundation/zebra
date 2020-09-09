use std::sync::Arc;
use zebra_chain::block::{self, Block};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A query about or modification to the chain state.
///
/// TODO: replace these variants with the ones in RFC5.
pub enum Request {
    // TODO(jlusby): deprecate in the future based on our validation story
    /// Add a block to the zebra-state
    AddBlock {
        /// The block to be added to the state
        block: Arc<Block>,
    },
    /// Get a block from the zebra-state
    GetBlock {
        /// The hash used to identify the block
        hash: block::Hash,
    },
    /// Get a block locator list for the current best chain
    GetBlockLocator {
        /// The genesis block of the current best chain
        genesis: block::Hash,
    },
    /// Get the block that is the tip of the current chain
    GetTip,
    /// Ask the state if the given hash is part of the current best chain
    GetDepth {
        /// The hash to check against the current chain
        hash: block::Hash,
    },
}
