//! History tree (Merkle mountain range) structure that contains information about
//! the block history as specified in ZIP-221.

mod tests;

use std::{
    collections::{BTreeMap, HashSet},
    io,
    ops::Deref,
    sync::Arc,
};

use thiserror::Error;

use crate::{
    block::{Block, ChainHistoryMmrRootHash, Height},
    fmt::SummaryDebug,
    orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::{Entry, Tree, V1 as PreOrchard, V2 as OrchardOnward},
    sapling,
};

/// An error describing why a history tree operation failed.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum HistoryTreeError {
    #[error("zcash_history error: {inner:?}")]
    #[non_exhaustive]
    InnerError { inner: zcash_history::Error },

    #[error("I/O error")]
    IOError(#[from] io::Error),
}

impl PartialEq for HistoryTreeError {
    fn eq(&self, other: &Self) -> bool {
        // Workaround since subtypes do not implement Eq.
        // This is only used for tests anyway.
        format!("{self:?}") == format!("{other:?}")
    }
}

impl Eq for HistoryTreeError {}

/// The inner [Tree] in one of its supported versions.
#[derive(Debug)]
enum InnerHistoryTree {
    /// A pre-Orchard tree.
    PreOrchard(Tree<PreOrchard>),
    /// An Orchard-onward tree.
    OrchardOnward(Tree<OrchardOnward>),
}

/// History tree (Merkle mountain range) structure that contains information about
/// the block history, as specified in [ZIP-221](https://zips.z.cash/zip-0221).
#[derive(Debug)]
pub struct NonEmptyHistoryTree {
    network: Network,
    network_upgrade: NetworkUpgrade,
    /// Merkle mountain range tree from `zcash_history`.
    /// This is a "runtime" structure used to add / remove nodes, and it's not
    /// persistent.
    inner: InnerHistoryTree,
    /// The number of nodes in the tree.
    size: u32,
    /// The peaks of the tree, indexed by their position in the array representation
    /// of the tree. This can be persisted to save the tree.
    peaks: SummaryDebug<BTreeMap<u32, Entry>>,
    /// The height of the most recent block added to the tree.
    current_height: Height,
}

impl NonEmptyHistoryTree {
    /// Recreate a [`HistoryTree`] from previously saved data.
    ///
    /// The parameters must come from the values of [`NonEmptyHistoryTree::size`],
    /// [`NonEmptyHistoryTree::peaks`] and [`NonEmptyHistoryTree::current_height`] of a HistoryTree.
    pub fn from_cache(
        network: &Network,
        size: u32,
        peaks: BTreeMap<u32, Entry>,
        current_height: Height,
    ) -> Result<Self, HistoryTreeError> {
        let network_upgrade = NetworkUpgrade::current(network, current_height);
        let inner = match network_upgrade {
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom => {
                panic!("HistoryTree does not exist for pre-Heartwood upgrades")
            }
            NetworkUpgrade::Heartwood | NetworkUpgrade::Canopy => {
                let tree = Tree::<PreOrchard>::new_from_cache(
                    network,
                    network_upgrade,
                    size,
                    &peaks,
                    &Default::default(),
                )?;
                InnerHistoryTree::PreOrchard(tree)
            }
            NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu7
            | NetworkUpgrade::Swap => {
                let tree = Tree::<OrchardOnward>::new_from_cache(
                    network,
                    network_upgrade,
                    size,
                    &peaks,
                    &Default::default(),
                )?;
                InnerHistoryTree::OrchardOnward(tree)
            }
        };
        Ok(Self {
            network: network.clone(),
            network_upgrade,
            inner,
            size,
            peaks: peaks.into(),
            current_height,
        })
    }

    /// Create a new history tree with a single block.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block;
    ///  (ignored for pre-Orchard blocks).
    #[allow(clippy::unwrap_in_result)]
    pub fn from_block(
        network: &Network,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<Self, HistoryTreeError> {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(network, height);
        let (tree, entry) = match network_upgrade {
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom => {
                panic!("HistoryTree does not exist for pre-Heartwood upgrades")
            }
            NetworkUpgrade::Heartwood | NetworkUpgrade::Canopy => {
                let (tree, entry) = Tree::<PreOrchard>::new_from_block(
                    network,
                    block,
                    sapling_root,
                    &Default::default(),
                )?;
                (InnerHistoryTree::PreOrchard(tree), entry)
            }
            NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu7
            | NetworkUpgrade::Swap => {
                let (tree, entry) = Tree::<OrchardOnward>::new_from_block(
                    network,
                    block,
                    sapling_root,
                    orchard_root,
                )?;
                (InnerHistoryTree::OrchardOnward(tree), entry)
            }
        };
        let mut peaks = BTreeMap::new();
        peaks.insert(0u32, entry);
        Ok(NonEmptyHistoryTree {
            network: network.clone(),
            network_upgrade,
            inner: tree,
            size: 1,
            peaks: peaks.into(),
            current_height: height,
        })
    }

    /// Add block data to the tree.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block;
    ///  (ignored for pre-Orchard blocks).
    ///
    /// # Panics
    ///
    /// If the block height is not one more than the previously pushed block.
    #[allow(clippy::unwrap_in_result)]
    pub fn push(
        &mut self,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<(), HistoryTreeError> {
        // Check if the block has the expected height.
        // librustzcash assumes the heights are correct and corrupts the tree if they are wrong,
        // resulting in a confusing error, which we prevent here.
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");

        assert!(
            Some(height) == self.current_height + 1,
            "added block with height {:?} but it must be {:?}+1",
            height,
            self.current_height
        );

        let network_upgrade = NetworkUpgrade::current(&self.network, height);
        if network_upgrade != self.network_upgrade {
            // This is the activation block of a network upgrade.
            // Create a new tree.
            let new_tree = Self::from_block(&self.network, block, sapling_root, orchard_root)?;
            // Replaces self with the new tree
            *self = new_tree;
            assert_eq!(self.network_upgrade, network_upgrade);
            return Ok(());
        }

        let new_entries = match &mut self.inner {
            InnerHistoryTree::PreOrchard(tree) => tree
                .append_leaf(block, sapling_root, orchard_root)
                .map_err(|e| HistoryTreeError::InnerError { inner: e })?,
            InnerHistoryTree::OrchardOnward(tree) => tree
                .append_leaf(block, sapling_root, orchard_root)
                .map_err(|e| HistoryTreeError::InnerError { inner: e })?,
        };
        for entry in new_entries {
            // Not every entry is a peak; those will be trimmed later
            self.peaks.insert(self.size, entry);
            self.size += 1;
        }
        self.prune()?;
        self.current_height = height;
        Ok(())
    }

    /// Extend the history tree with the given blocks.
    pub fn try_extend<
        'a,
        T: IntoIterator<Item = (Arc<Block>, &'a sapling::tree::Root, &'a orchard::tree::Root)>,
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
        // This code is based on a librustzcash example:
        // https://github.com/zcash/librustzcash/blob/02052526925fba9389f1428d6df254d4dec967e6/zcash_history/examples/long.rs
        // The explanation of how it works is from zcashd:
        // https://github.com/zcash/zcash/blob/0247c0c682d59184a717a6536edb0d18834be9a7/src/coins.cpp#L351

        let mut peak_pos_set = HashSet::new();

        // Assume the following example peak layout with 14 leaves, and 25 stored nodes in
        // total (the "tree length"):
        //
        //             P
        //            /\
        //           /  \
        //          / \  \
        //        /    \  \  Altitude
        //     _A_      \  \    3
        //   _/   \_     B  \   2
        //  / \   / \   / \  C  1
        // /\ /\ /\ /\ /\ /\ /\ 0
        //
        // We start by determining the altitude of the highest peak (A).
        let mut alt = (32 - (self.size + 1).leading_zeros() - 1) - 1;

        // We determine the position of the highest peak (A) by pretending it is the right
        // sibling in a tree, and its left-most leaf has position 0. Then the left sibling
        // of (A) has position -1, and so we can "jump" to the peak's position by computing
        // -1 + 2^(alt + 1) - 1.
        let mut peak_pos = (1 << (alt + 1)) - 2;

        // Now that we have the position and altitude of the highest peak (A), we collect
        // the remaining peaks (B, C). We navigate the peaks as if they were nodes in this
        // Merkle tree (with additional imaginary nodes 1 and 2, that have positions beyond
        // the MMR's length):
        //
        //             / \
        //            /   \
        //           /     \
        //         /         \
        //       A ==========> 1
        //      / \          //  \
        //    _/   \_       B ==> 2
        //   /\     /\     /\    //
        //  /  \   /  \   /  \   C
        // /\  /\ /\  /\ /\  /\ /\
        //
        loop {
            // If peak_pos is out of bounds of the tree, we compute the position of its left
            // child, and drop down one level in the tree.
            if peak_pos >= self.size {
                // left child, -2^alt
                peak_pos -= 1 << alt;
                alt -= 1;
            }

            // If the peak exists, we take it and then continue with its right sibling.
            if peak_pos < self.size {
                // There is a peak at index `peak_pos`
                peak_pos_set.insert(peak_pos);

                // right sibling
                peak_pos = peak_pos + (1 << (alt + 1)) - 1;
            }

            if alt == 0 {
                break;
            }
        }

        // Remove all non-peak entries
        self.peaks.retain(|k, _| peak_pos_set.contains(k));
        // Rebuild tree
        self.inner = match self.inner {
            InnerHistoryTree::PreOrchard(_) => {
                InnerHistoryTree::PreOrchard(Tree::<PreOrchard>::new_from_cache(
                    &self.network,
                    self.network_upgrade,
                    self.size,
                    &self.peaks,
                    &Default::default(),
                )?)
            }
            InnerHistoryTree::OrchardOnward(_) => {
                InnerHistoryTree::OrchardOnward(Tree::<OrchardOnward>::new_from_cache(
                    &self.network,
                    self.network_upgrade,
                    self.size,
                    &self.peaks,
                    &Default::default(),
                )?)
            }
        };
        Ok(())
    }

    /// Return the hash of the tree root.
    pub fn hash(&self) -> ChainHistoryMmrRootHash {
        match &self.inner {
            InnerHistoryTree::PreOrchard(tree) => tree.hash(),
            InnerHistoryTree::OrchardOnward(tree) => tree.hash(),
        }
    }

    /// Return the peaks of the tree.
    pub fn peaks(&self) -> &BTreeMap<u32, Entry> {
        &self.peaks
    }

    /// Return the (total) number of nodes in the tree.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Return the height of the last added block.
    pub fn current_height(&self) -> Height {
        self.current_height
    }

    /// Return the network where this tree is used.
    pub fn network(&self) -> &Network {
        &self.network
    }
}

impl Clone for NonEmptyHistoryTree {
    fn clone(&self) -> Self {
        let tree = match self.inner {
            InnerHistoryTree::PreOrchard(_) => InnerHistoryTree::PreOrchard(
                Tree::<PreOrchard>::new_from_cache(
                    &self.network,
                    self.network_upgrade,
                    self.size,
                    &self.peaks,
                    &Default::default(),
                )
                .expect("rebuilding an existing tree should always work"),
            ),
            InnerHistoryTree::OrchardOnward(_) => InnerHistoryTree::OrchardOnward(
                Tree::<OrchardOnward>::new_from_cache(
                    &self.network,
                    self.network_upgrade,
                    self.size,
                    &self.peaks,
                    &Default::default(),
                )
                .expect("rebuilding an existing tree should always work"),
            ),
        };
        NonEmptyHistoryTree {
            network: self.network.clone(),
            network_upgrade: self.network_upgrade,
            inner: tree,
            size: self.size,
            peaks: self.peaks.clone(),
            current_height: self.current_height,
        }
    }
}

/// A History Tree that keeps track of its own creation in the Heartwood
/// activation block, being empty beforehand.
#[derive(Debug, Default, Clone)]
pub struct HistoryTree(Option<NonEmptyHistoryTree>);

impl HistoryTree {
    /// Create a HistoryTree from a block.
    ///
    /// If the block is pre-Heartwood, it returns an empty history tree.
    #[allow(clippy::unwrap_in_result)]
    pub fn from_block(
        network: &Network,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<Self, HistoryTreeError> {
        let Some(heartwood_height) = NetworkUpgrade::Heartwood.activation_height(network) else {
            // Return early if there is no Heartwood activation height.
            return Ok(HistoryTree(None));
        };

        match block
            .coinbase_height()
            .expect("must have height")
            .cmp(&heartwood_height)
        {
            std::cmp::Ordering::Less => Ok(HistoryTree(None)),
            _ => Ok(
                NonEmptyHistoryTree::from_block(network, block, sapling_root, orchard_root)?.into(),
            ),
        }
    }

    /// Push a block to a maybe-existing HistoryTree, handling network upgrades.
    ///
    /// The tree is updated in-place. It is created when pushing the Heartwood
    /// activation block.
    #[allow(clippy::unwrap_in_result)]
    pub fn push(
        &mut self,
        network: &Network,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<(), HistoryTreeError> {
        let Some(heartwood_height) = NetworkUpgrade::Heartwood.activation_height(network) else {
            assert!(
                self.0.is_none(),
                "history tree must not exist pre-Heartwood"
            );

            return Ok(());
        };

        match block
            .coinbase_height()
            .expect("must have height")
            .cmp(&heartwood_height)
        {
            std::cmp::Ordering::Less => {
                assert!(
                    self.0.is_none(),
                    "history tree must not exist pre-Heartwood"
                );
            }
            std::cmp::Ordering::Equal => {
                let tree = Some(NonEmptyHistoryTree::from_block(
                    network,
                    block,
                    sapling_root,
                    orchard_root,
                )?);
                // Replace the current object with the new tree
                *self = HistoryTree(tree);
            }
            std::cmp::Ordering::Greater => {
                self.0
                    .as_mut()
                    .expect("history tree must exist Heartwood-onward")
                    .push(block.clone(), sapling_root, orchard_root)?;
            }
        };
        Ok(())
    }

    /// Return the hash of the tree root if the tree is not empty.
    pub fn hash(&self) -> Option<ChainHistoryMmrRootHash> {
        Some(self.0.as_ref()?.hash())
    }
}

impl From<NonEmptyHistoryTree> for HistoryTree {
    fn from(tree: NonEmptyHistoryTree) -> Self {
        HistoryTree(Some(tree))
    }
}

impl From<Option<NonEmptyHistoryTree>> for HistoryTree {
    fn from(tree: Option<NonEmptyHistoryTree>) -> Self {
        HistoryTree(tree)
    }
}

impl Deref for HistoryTree {
    type Target = Option<NonEmptyHistoryTree>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for HistoryTree {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref().map(|tree| tree.hash()) == other.as_ref().map(|other_tree| other_tree.hash())
    }
}

impl Eq for HistoryTree {}
