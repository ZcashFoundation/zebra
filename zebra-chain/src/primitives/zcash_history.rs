//! Contains code that interfaces with the zcash_history crate from
//! librustzcash.

// TODO: remove after this module gets to be used
#![allow(missing_docs)]

mod tests;

use std::{collections::BTreeMap, io, sync::Arc};

use serde_big_array::BigArray;
pub use zcash_history::{V1, V2};

use crate::{
    block::{Block, ChainHistoryMmrRootHash},
    orchard,
    parameters::{Network, NetworkUpgrade},
    sapling,
};

/// A trait to represent a version of `Tree`.
pub trait Version: zcash_history::Version {
    /// Convert a Block into the NodeData for this version.
    fn block_to_history_node(
        block: Arc<Block>,
        network: &Network,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Self::NodeData;
}

/// A MMR Tree using zcash_history::Tree.
///
/// Currently it should not be used as a long-term data structure because it
/// may grow without limits.
pub struct Tree<V: zcash_history::Version> {
    network: Network,
    network_upgrade: NetworkUpgrade,
    inner: zcash_history::Tree<V>,
}

/// An encoded tree node data.
pub struct NodeData {
    inner: [u8; zcash_history::MAX_NODE_DATA_SIZE],
}

impl From<&zcash_history::NodeData> for NodeData {
    /// Convert from librustzcash.
    fn from(inner_node: &zcash_history::NodeData) -> Self {
        let mut node = NodeData {
            inner: [0; zcash_history::MAX_NODE_DATA_SIZE],
        };
        inner_node
            .write(&mut &mut node.inner[..])
            .expect("buffer has the proper size");
        node
    }
}

/// An encoded entry in the tree.
///
/// Contains the node data and information about its position in the tree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(with = "BigArray")]
    inner: [u8; zcash_history::MAX_ENTRY_SIZE],
}

impl Entry {
    /// Create a leaf Entry for the given block, its network, and the root of its
    /// note commitment trees.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block;
    ///  (ignored for V1 trees).
    fn new_leaf<V: Version>(
        block: Arc<Block>,
        network: &Network,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Self {
        let node_data = V::block_to_history_node(block, network, sapling_root, orchard_root);
        let inner_entry = zcash_history::Entry::<V>::new_leaf(node_data);
        let mut entry = Entry {
            inner: [0; zcash_history::MAX_ENTRY_SIZE],
        };
        inner_entry
            .write(&mut &mut entry.inner[..])
            .expect("buffer has the proper size");
        entry
    }
}

impl<V: Version> Tree<V> {
    /// Create a MMR tree with the given length from the given cache of nodes.
    ///
    /// The `peaks` are the peaks of the MMR tree to build and their position in the
    /// array representation of the tree.
    /// The `extra` are extra nodes that enable removing nodes from the tree, and their position.
    ///
    /// Note that the length is usually larger than the length of `peaks` and `extra`, since
    /// you don't need to pass every node, just the peaks of the tree (plus extra).
    ///
    /// # Panics
    ///
    /// Will panic if `peaks` is empty.
    #[allow(clippy::unwrap_in_result)]
    pub fn new_from_cache(
        network: &Network,
        network_upgrade: NetworkUpgrade,
        length: u32,
        peaks: &BTreeMap<u32, Entry>,
        extra: &BTreeMap<u32, Entry>,
    ) -> Result<Self, io::Error> {
        let branch_id = network_upgrade
            .branch_id()
            .expect("unexpected pre-Overwinter MMR history tree");
        let mut peaks_vec = Vec::new();
        for (idx, entry) in peaks {
            let inner_entry = zcash_history::Entry::from_bytes(branch_id.into(), entry.inner)?;
            peaks_vec.push((*idx, inner_entry));
        }
        let mut extra_vec = Vec::new();
        for (idx, entry) in extra {
            let inner_entry = zcash_history::Entry::from_bytes(branch_id.into(), entry.inner)?;
            extra_vec.push((*idx, inner_entry));
        }
        let inner = zcash_history::Tree::new(length, peaks_vec, extra_vec);
        Ok(Tree {
            network: network.clone(),
            network_upgrade,
            inner,
        })
    }

    /// Create a single-node MMR tree for the given block.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block;
    ///  (ignored for V1 trees).
    #[allow(clippy::unwrap_in_result)]
    pub fn new_from_block(
        network: &Network,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<(Self, Entry), io::Error> {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(network, height);
        let entry0 = Entry::new_leaf::<V>(block, network, sapling_root, orchard_root);
        let mut peaks = BTreeMap::new();
        peaks.insert(0u32, entry0);
        Ok((
            Tree::new_from_cache(network, network_upgrade, 1, &peaks, &BTreeMap::new())?,
            peaks
                .remove(&0u32)
                .expect("must work since it was just added"),
        ))
    }

    /// Append a new block to the tree, as a new leaf.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block;
    ///  (ignored for V1 trees).
    ///
    /// Returns a vector of nodes added to the tree (leaf + internal nodes).
    ///
    /// # Panics
    ///
    /// Panics if the network upgrade of the given block is different from
    /// the network upgrade of the other blocks in the tree.
    #[allow(clippy::unwrap_in_result)]
    pub fn append_leaf(
        &mut self,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Result<Vec<Entry>, zcash_history::Error> {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(&self.network, height);

        assert!(
            network_upgrade == self.network_upgrade,
            "added block from network upgrade {:?} but history tree is restricted to {:?}",
            network_upgrade,
            self.network_upgrade
        );

        let node_data = V::block_to_history_node(block, &self.network, sapling_root, orchard_root);
        let appended = self.inner.append_leaf(node_data)?;

        let mut new_nodes = Vec::new();
        for entry_link in appended {
            let mut entry = Entry {
                inner: [0; zcash_history::MAX_ENTRY_SIZE],
            };
            self.inner
                .resolve_link(entry_link)
                .expect("entry was just generated so it must be valid")
                .node()
                .write(&mut &mut entry.inner[..])
                .expect("buffer was created with enough capacity");
            new_nodes.push(entry);
        }
        Ok(new_nodes)
    }
    /// Return the root hash of the tree, i.e. `hashChainHistoryRoot`.
    pub fn hash(&self) -> ChainHistoryMmrRootHash {
        // Both append_leaf() and truncate_leaf() leave a root node, so it should
        // always exist.
        V::hash(self.inner.root_node().expect("must have root node").data()).into()
    }
}

impl<V: zcash_history::Version> std::fmt::Debug for Tree<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree")
            .field("network", &self.network)
            .field("network_upgrade", &self.network_upgrade)
            .finish()
    }
}

impl Version for zcash_history::V1 {
    /// Convert a Block into a V1::NodeData used in the MMR tree.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is ignored.
    fn block_to_history_node(
        block: Arc<Block>,
        network: &Network,
        sapling_root: &sapling::tree::Root,
        _orchard_root: &orchard::tree::Root,
    ) -> Self::NodeData {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(network, height);
        let branch_id = network_upgrade
            .branch_id()
            .expect("must have branch ID for chain history network upgrades");
        let block_hash = block.hash().0;
        let time: u32 = block
            .header
            .time
            .timestamp()
            .try_into()
            .expect("deserialized and generated timestamps are u32 values");
        let target = block.header.difficulty_threshold.0;
        let sapling_root: [u8; 32] = sapling_root.into();
        let work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work must be valid during contextual verification");
        // There is no direct `std::primitive::u128` to `bigint::U256` conversion
        let work = primitive_types::U256::from_big_endian(&work.as_u128().to_be_bytes());

        let sapling_tx_count = block.sapling_transactions_count();

        match network_upgrade {
            NetworkUpgrade::Genesis
            | NetworkUpgrade::BeforeOverwinter
            | NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom => {
                panic!("HistoryTree does not exist for pre-Heartwood upgrades")
            }
            // Nu5 is included because this function is called by the V2 implementation
            // since the V1::NodeData is included inside the V2::NodeData.
            NetworkUpgrade::Heartwood
            | NetworkUpgrade::Canopy
            | NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu7 => zcash_history::NodeData {
                consensus_branch_id: branch_id.into(),
                subtree_commitment: block_hash,
                start_time: time,
                end_time: time,
                start_target: target,
                end_target: target,
                start_sapling_root: sapling_root,
                end_sapling_root: sapling_root,
                subtree_total_work: work,
                start_height: height.0 as u64,
                end_height: height.0 as u64,
                sapling_tx: sapling_tx_count,
            },
        }
    }
}

impl Version for V2 {
    /// Convert a Block into a V1::NodeData used in the MMR tree.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    /// `orchard_root` is the root of the Orchard note commitment tree of the block.
    fn block_to_history_node(
        block: Arc<Block>,
        network: &Network,
        sapling_root: &sapling::tree::Root,
        orchard_root: &orchard::tree::Root,
    ) -> Self::NodeData {
        let orchard_tx_count = block.orchard_transactions_count();
        let node_data_v1 = V1::block_to_history_node(block, network, sapling_root, orchard_root);
        let orchard_root: [u8; 32] = orchard_root.into();
        Self::NodeData {
            v1: node_data_v1,
            start_orchard_root: orchard_root,
            end_orchard_root: orchard_root,
            orchard_tx: orchard_tx_count,
        }
    }
}
