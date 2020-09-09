use std::sync::Arc;
use zebra_chain::block::{self, Block};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a state [`Request`](super::Request).
pub enum Response {
    /// The response to a `AddBlock` request indicating a block was successfully
    /// added to the state
    Added {
        /// The hash of the block that was added
        hash: block::Hash,
    },
    /// The response to a `GetBlock` request by hash
    Block {
        /// The block that was requested
        block: Arc<Block>,
    },
    /// The response to a `GetBlockLocator` request
    BlockLocator {
        /// The set of blocks that make up the block locator
        block_locator: Vec<block::Hash>,
    },
    /// The response to a `GetTip` request
    Tip {
        /// The hash of the block at the tip of the current chain
        hash: block::Hash,
    },
    /// The response to a `Contains` request indicating that the given has is in
    /// the current best chain
    Depth(
        /// The number of blocks above the given block in the current best chain
        Option<u32>,
    ),
}
