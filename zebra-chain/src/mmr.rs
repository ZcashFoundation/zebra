//! History tree (Merkle mountain range) structure that contains information about
//! the block history as specified in ZIP-221.

use std::{collections::HashMap, sync::Arc};

use crate::{
    block::{Block, ChainHistoryMmrRootHash},
    orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::{Entry, Tree},
    sapling,
};

/// History tree structure.
pub struct HistoryTree {
    network: Network,
    network_upgrade: NetworkUpgrade,
    // Merkle mountain range tree.
    // This is a "runtime" structure used to add / remove nodes, and it's not
    // persistent.
    inner: Tree,
    // The number of nodes in the tree.
    size: u32,
    // The peaks of the tree, indexed by their position in the array representation
    // of the tree. This can be persisted to save the tree.
    // TODO: use NodeData instead of Entry to save space? Requires re-deriving
    // the entry metadata from its position.
    peaks: HashMap<u32, Entry>,
}

impl HistoryTree {
    /// Create a new history tree with a single block.
    pub fn new_from_block(
        network: Network,
        network_upgrade: NetworkUpgrade,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        _orchard_root: Option<&orchard::tree::Root>,
    ) -> Self {
        let (tree, entry) =
            Tree::new_from_block(network, block, sapling_root).expect("TODO: handle error");
        let mut peaks = HashMap::new();
        peaks.insert(0u32, entry);
        HistoryTree {
            network,
            network_upgrade,
            inner: tree,
            size: 1,
            peaks,
        }
    }

    /// Add block data to the tree.
    pub fn push(
        &mut self,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        _orchard_root: Option<&orchard::tree::Root>,
    ) {
        // TODO: handle orchard root
        let new_entries = self
            .inner
            .append_leaf(block, sapling_root)
            .expect("TODO: handle error");
        for entry in new_entries {
            // Not every entry is a peak; those will be trimmed later
            self.peaks.insert(self.size, entry);
            self.size += 1;
        }
        // TODO: trim entries?
        // TODO: rebuild Tree from the peaks? When / how often?
        // (zcashd rebuilds it on every operation)
    }

    /// Return the hash of the tree root.
    pub fn hash(&self) -> ChainHistoryMmrRootHash {
        self.inner.hash()
    }
}

impl Clone for HistoryTree {
    fn clone(&self) -> Self {
        let tree = Tree::new_from_cache(
            self.network,
            self.network_upgrade,
            self.size,
            &self.peaks,
            &HashMap::new(),
        )
        .expect("rebuilding an existing tree should always work");
        HistoryTree {
            network: self.network,
            network_upgrade: self.network_upgrade,
            inner: tree,
            size: self.size,
            peaks: self.peaks.clone(),
        }
    }
}

impl<'a>
    Extend<(
        Arc<Block>,
        &'a sapling::tree::Root,
        Option<&'a orchard::tree::Root>,
    )> for HistoryTree
{
    /// Extend the history tree with the given blocks.
    fn extend<
        T: IntoIterator<
            Item = (
                Arc<Block>,
                &'a sapling::tree::Root,
                Option<&'a orchard::tree::Root>,
            ),
        >,
    >(
        &mut self,
        iter: T,
    ) {
        for (block, sapling_root, orchard_root) in iter {
            // TODO: handle errors; give up on using Extend trait?
            self.push(block, &sapling_root, orchard_root);
        }
    }
}
