//! History tree (Merkle mountain range) structure that contains information about
//! the block history as specified in ZIP-221.

use std::sync::Arc;

use crate::block::{Block, ChainHistoryMmrRootHash};

/// History tree structure.
pub struct HistoryTree {
    // TODO
}

impl HistoryTree {
    /// Add block data to the tree.
    pub fn push(&mut self, _block: &Block) {
        // TODO: zcash_history::Tree::append_leaf receives an Arc<Block>. Should
        // change that to match, or create an Arc to pass to it?
        todo!();
    }

    /// Return the hash of the tree root.
    pub fn hash(&self) -> ChainHistoryMmrRootHash {
        todo!();
    }
}

impl Clone for HistoryTree {
    fn clone(&self) -> Self {
        todo!();
    }
}

impl Extend<Arc<Block>> for HistoryTree {
    /// Extend the history tree with the given blocks.
    // TODO: add Sapling and Orchard roots to parameters
    fn extend<T: IntoIterator<Item = Arc<Block>>>(&mut self, _iter: T) {
        todo!()
    }
}
