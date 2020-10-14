use std::sync::Arc;
use zebra_chain::{
    block::{self, Block},
    transaction, transparent,
};

// Allow *only* this unused import, so that rustdoc link resolution
// will work with inline links.
#[allow(unused_imports)]
use crate::Response;

/// Identify a block by hash or height.
///
/// This enum implements `From` for [`block::Hash`] and [`block::Height`],
/// so it can be created using `hash.into()` or `height.into()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashOrHeight {
    /// A block identified by hash.
    Hash(block::Hash),
    /// A block identified by height.
    Height(block::Height),
}

impl From<block::Hash> for HashOrHeight {
    fn from(hash: block::Hash) -> Self {
        Self::Hash(hash)
    }
}

impl From<block::Height> for HashOrHeight {
    fn from(hash: block::Height) -> Self {
        Self::Height(hash)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A query about or modification to the chain state.
pub enum Request {
    /// Performs contextual validation of the given block, committing it to the
    /// state if successful.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly
    /// committed block, or an error.
    ///
    /// This request can be made out-of-order; the state service will buffer it
    /// until its parent is ready.
    CommitBlock {
        /// The block to commit to the state.
        block: Arc<Block>,
        // TODO: add these parameters when we can compute anchors.
        // sprout_anchor: sprout::tree::Root,
        // sapling_anchor: sapling::tree::Root,
    },

    /// Commit a finalized block to the state, skipping contextual validation.
    /// This is exposed for use in checkpointing, which produces finalized
    /// blocks.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly
    /// committed block, or an error.
    ///
    /// This request can be made out-of-order; the state service will buffer it
    /// until its parent is ready.
    CommitFinalizedBlock {
        /// The block to commit to the state.
        block: Arc<Block>,
        // TODO: add these parameters when we can compute anchors.
        // sprout_anchor: sprout::tree::Root,
        // sapling_anchor: sapling::tree::Root,
    },

    /// Computes the depth in the best chain of the block identified by the given hash.
    ///
    /// Returns
    ///
    /// * [`Response::Depth(Some(depth))`](Response::Depth) if the block is in the main chain;
    /// * [`Response::Depth(None)`](Response::Depth) otherwise.
    Depth(block::Hash),

    /// Returns [`Response::Tip`] with the current best chain tip.
    Tip,

    /// Computes a block locator object based on the current chain state.
    ///
    /// Returns [`Response::BlockLocator`] with hashes starting
    /// from the current chain tip and reaching backwards towards the genesis
    /// block. The first hash is the current chain tip. The last hash is the tip
    /// of the finalized portion of the state. If the state is empty, the block
    /// locator is also empty.
    BlockLocator,

    /// Looks up a transaction by hash.
    ///
    /// Returns
    ///
    /// * [`Response::Transaction(Some(Arc<Transaction>))`](Response::Transaction) if the transaction is known;
    /// * [`Response::Transaction(None)`](Response::Transaction) otherwise.
    Transaction(transaction::Hash),

    /// Looks up a block by hash or height.
    ///
    /// Returns
    ///
    /// * [`Response::Block(Some(Arc<Block>))`](Response::Block) if the block is known;
    /// * [`Response::Block(None)`](Response::Transaction) otherwise.
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    Block(HashOrHeight),

    /// Request a UTXO identified by the given Outpoint
    AwaitUtxo(transparent::OutPoint),
}
