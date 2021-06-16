//! Merkle mountain range structure that contains information about
//! the block history as specified in ZIP-221.

use std::borrow::Borrow;

use crate::block::{Block, ChainHistoryMmrRootHash};

/// Merkle mountain range structure.
pub struct MerkleMountainRange {
    // TODO
}

impl MerkleMountainRange {
    pub fn push(&mut self, block: &Block) {
        // TODO: zcash_history::Tree::append_leaf receives an Arc<Block>. Should
        // change that to match, or create an Arc to pass to it?
        todo!();
    }

    /// Extend the MMR with the given blocks.
    // TODO: add Sapling and Orchard roots to parameters
    pub fn extend<C>(&mut self, _vals: &C)
    where
        C: IntoIterator,
        C::Item: Borrow<Block>,
        C::IntoIter: ExactSizeIterator,
    {
        // TODO: How to convert C to `impl Iterator<Item = (Arc<Block>, sapling::tree::Root)>`
        // for zcash_history::Tree::append_leaf_iter? Should change that to match C?
        todo!();
    }

    /// Return the hash of the MMR root.
    pub fn hash(&self) -> ChainHistoryMmrRootHash {
        todo!();
    }
}

impl Clone for MerkleMountainRange {
    fn clone(&self) -> Self {
        todo!();
    }
}
