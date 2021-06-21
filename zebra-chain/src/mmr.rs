//! History tree (Merkle mountain range) structure that contains information about
//! the block history as specified in ZIP-221.

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::Arc,
};

use thiserror::Error;

use crate::{
    block::{Block, ChainHistoryMmrRootHash},
    orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::{Entry, Tree},
    sapling,
};

/// An error describing why a history tree operation failed.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum HistoryTreeError {
    #[error("error from the underlying library: {inner:?}")]
    #[non_exhaustive]
    InnerError { inner: zcash_history::Error },

    #[error("I/O error")]
    IOError(#[from] io::Error),
}

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
    ) -> Result<Self, io::Error> {
        // TODO: handle Orchard root
        let (tree, entry) = Tree::new_from_block(network, block, sapling_root)?;
        let mut peaks = HashMap::new();
        peaks.insert(0u32, entry);
        Ok(HistoryTree {
            network,
            network_upgrade,
            inner: tree,
            size: 1,
            peaks,
        })
    }

    /// Add block data to the tree.
    pub fn push(
        &mut self,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        _orchard_root: Option<&orchard::tree::Root>,
    ) -> Result<(), HistoryTreeError> {
        // TODO: handle orchard root
        let new_entries = self
            .inner
            .append_leaf(block, sapling_root)
            .map_err(|e| HistoryTreeError::InnerError { inner: e })?;
        for entry in new_entries {
            // Not every entry is a peak; those will be trimmed later
            self.peaks.insert(self.size, entry);
            self.size += 1;
        }
        // TODO: should we really prune every iteration?
        self.prune()?;
        // TODO: implement network upgrade logic: drop previous history, start new history
        Ok(())
    }

    /// Extend the history tree with the given blocks.
    pub fn extend<
        'a,
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
    ) -> Result<(), HistoryTreeError> {
        for (block, sapling_root, orchard_root) in iter {
            self.push(block, sapling_root, orchard_root)?;
        }
        Ok(())
    }

    /// Prune tree, removing all non-peak entries.
    fn prune(&mut self) -> Result<(), io::Error> {
        // Go through all the peaks of the tree.
        // This code is from a librustzcash example.
        // https://github.com/zcash/librustzcash/blob/02052526925fba9389f1428d6df254d4dec967e6/zcash_history/examples/long.rs
        // See also coins.cpp in zcashd for a explanation of how it works.
        // https://github.com/zcash/zcash/blob/0247c0c682d59184a717a6536edb0d18834be9a7/src/coins.cpp#L351

        // TODO: should we just copy the explanation here?

        let mut peak_pos_set = HashSet::new();
        // integer log2 of (self.size+1), -1
        let mut h = (32 - ((self.size + 1) as u32).leading_zeros() - 1) - 1;
        let mut peak_pos = (1 << (h + 1)) - 1;

        loop {
            if peak_pos > self.size {
                // left child, -2^h
                peak_pos -= 1 << h;
                h -= 1;
            }

            if peak_pos <= self.size {
                // There is a peak at index `peak_pos`
                peak_pos_set.insert(peak_pos);

                // right sibling
                peak_pos = peak_pos + (1 << (h + 1)) - 1;
            }

            if h == 0 {
                break;
            }
        }

        // Remove all non-peak entries
        self.peaks.retain(|k, _| peak_pos_set.contains(k));
        // Rebuild tree
        self.inner = Tree::new_from_cache(
            self.network,
            self.network_upgrade,
            self.size,
            &self.peaks,
            &Default::default(),
        )?;
        Ok(())
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
