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
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HashOrHeight {
    /// A block identified by hash.
    Hash(block::Hash),
    /// A block identified by height.
    Height(block::Height),
}

impl HashOrHeight {
    /// Unwrap the inner height or attempt to retrieve the height for a given
    /// hash if one exists.
    pub fn height_or_else<F>(self, op: F) -> Option<block::Height>
    where
        F: FnOnce(block::Hash) -> Option<block::Height>,
    {
        match self {
            HashOrHeight::Hash(hash) => op(hash),
            HashOrHeight::Height(height) => Some(height),
        }
    }
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
    /// It is the caller's responsibility to perform semantic validation. This
    /// request can be made out-of-order; the state service will queue it until
    /// its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the block when it is
    /// committed to the state, or an error if the block fails contextual
    /// validation or has already been committed to the state.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed. A
    /// request to commit a block which has been queued internally but not yet
    /// committed will fail the older request and replace it with the newer request.
    CommitBlock {
        /// The block to commit to the state.
        block: Arc<Block>,
        // TODO: add these parameters when we can compute anchors.
        // sprout_anchor: sprout::tree::Root,
        // sapling_anchor: sapling::tree::Root,
    },

    /// Commit a finalized block to the state, skipping all validation.
    ///
    /// This is exposed for use in checkpointing, which produces finalized
    /// blocks. It is the caller's responsibility to ensure that the block is
    /// valid and final. This request can be made out-of-order; the state service
    /// will queue it until its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly committed
    /// block, or an error.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed.
    /// Duplicate requests should not be made, because it is the caller's
    /// responsibility to ensure that each block is valid and final.
    CommitFinalizedBlock {
        /// The block to commit to the state.
        block: Arc<Block>,
        // TODO: add these parameters when we can compute anchors.
        // sprout_anchor: sprout::tree::Root,
        // sapling_anchor: sapling::tree::Root,
    },

    /// Computes the depth in the current best chain of the block identified by the given hash.
    ///
    /// Returns
    ///
    /// * [`Response::Depth(Some(depth))`](Response::Depth) if the block is in the best chain;
    /// * [`Response::Depth(None)`](Response::Depth) otherwise.
    Depth(block::Hash),

    /// Returns [`Response::Tip`] with the current best chain tip.
    Tip,

    /// Computes a block locator object based on the current best chain.
    ///
    /// Returns [`Response::BlockLocator`] with hashes starting
    /// from the best chain tip, and following the chain of previous
    /// hashes. The first hash is the best chain tip. The last hash is
    /// the tip of the finalized portion of the state. Block locators
    /// are not continuous - some intermediate hashes might be skipped.
    ///
    /// If the state is empty, the block locator is also empty.
    BlockLocator,

    /// Looks up a transaction by hash in the current best chain.
    ///
    /// Returns
    ///
    /// * [`Response::Transaction(Some(Arc<Transaction>))`](Response::Transaction) if the transaction is in the best chain;
    /// * [`Response::Transaction(None)`](Response::Transaction) otherwise.
    Transaction(transaction::Hash),

    /// Looks up a block by hash or height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`Response::Block(Some(Arc<Block>))`](Response::Block) if the block is in the best chain;
    /// * [`Response::Block(None)`](Response::Block) otherwise.
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    Block(HashOrHeight),

    /// Request a UTXO identified by the given Outpoint, waiting until it becomes
    /// available if it is unknown.
    ///
    /// This request is purely informational, and there are no guarantees about
    /// whether the UTXO remains unspent or is on the best chain. Its purpose is
    /// to allow asynchronous script verification.
    ///
    /// Code making this request should apply a timeout layer to the service to
    /// handle missing UTXOs.
    AwaitUtxo(transparent::OutPoint),
}
