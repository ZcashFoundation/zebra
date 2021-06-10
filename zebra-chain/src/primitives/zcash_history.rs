//! Contains code that interfaces with the zcash_history crate from
//! librustzcash.

// TODO: remove after this module gets to be used
#![allow(dead_code)]

use std::{collections::HashMap, convert::TryInto, io, sync::Arc};

use crate::{
    block::{Block, ChainHistoryMmrRootHash},
    parameters::{ConsensusBranchId, Network, NetworkUpgrade},
    sapling,
};

/// A MMR Tree using zcash_history::Tree.
///
/// Currently it should not be used as a long-term data structure because it
/// may grow without limits.
pub struct Tree {
    network: Network,
    network_upgrade: NetworkUpgrade,
    inner: zcash_history::Tree,
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
/// Contains the node data and information about its position in the tree.
pub struct Entry {
    inner: [u8; zcash_history::MAX_ENTRY_SIZE],
}

impl From<zcash_history::Entry> for Entry {
    /// Convert from librustzcash.
    fn from(inner_entry: zcash_history::Entry) -> Self {
        let mut entry = Entry {
            inner: [0; zcash_history::MAX_ENTRY_SIZE],
        };
        inner_entry
            .write(&mut &mut entry.inner[..])
            .expect("buffer has the proper size");
        entry
    }
}

impl Entry {
    /// Create a leaf Entry for the given block, its network, and the root of its
    /// Sapling note commitment tree.
    fn new_leaf(block: Arc<Block>, network: Network, sapling_root: &sapling::tree::Root) -> Self {
        let node_data = block_to_history_node(block, network, sapling_root);
        let inner_entry: zcash_history::Entry = node_data.into();
        inner_entry.into()
    }

    /// Create a node (non-leaf) Entry from the encoded node data and the indices of
    /// its children (in the array representation of the MMR tree).
    fn new_node(
        branch_id: ConsensusBranchId,
        data: NodeData,
        left_idx: u32,
        right_idx: u32,
    ) -> Result<Self, io::Error> {
        let node_data = zcash_history::NodeData::from_bytes(branch_id.into(), data.inner)?;
        let inner_entry = zcash_history::Entry::new(
            node_data,
            zcash_history::EntryLink::Stored(left_idx),
            zcash_history::EntryLink::Stored(right_idx),
        );
        Ok(inner_entry.into())
    }
}

impl Tree {
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
    fn new_from_cache(
        network: Network,
        network_upgrade: NetworkUpgrade,
        length: u32,
        peaks: &HashMap<u32, &Entry>,
        extra: &HashMap<u32, &Entry>,
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
            network,
            network_upgrade,
            inner,
        })
    }

    /// Create a single-node MMR tree for the given block.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    fn new_from_block(
        network: Network,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
    ) -> Result<Self, io::Error> {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(network, height);
        let entry0 = Entry::new_leaf(block, network, sapling_root);
        let mut peaks = HashMap::new();
        peaks.insert(0u32, &entry0);
        Tree::new_from_cache(network, network_upgrade, 1, &peaks, &HashMap::new())
    }

    /// Append a new block to the tree, as a new leaf.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    ///
    /// Returns a vector of nodes added to the tree (leaf + internal nodes).
    ///
    /// # Panics
    ///
    /// Panics if the network upgrade of the given block is different from
    /// the network upgrade of the other blocks in the tree.
    fn append_leaf(
        &mut self,
        block: Arc<Block>,
        sapling_root: &sapling::tree::Root,
    ) -> Result<Vec<NodeData>, zcash_history::Error> {
        let height = block
            .coinbase_height()
            .expect("block must have coinbase height during contextual verification");
        let network_upgrade = NetworkUpgrade::current(self.network, height);
        if self.network_upgrade != network_upgrade {
            panic!(
                "added block from network upgrade {:?} but MMR tree is restricted to {:?}",
                network_upgrade, self.network_upgrade
            );
        }

        let node_data = block_to_history_node(block, self.network, sapling_root);
        let appended = self.inner.append_leaf(node_data)?;

        let mut new_nodes = Vec::new();
        for entry in appended {
            let mut node = NodeData {
                inner: [0; zcash_history::MAX_NODE_DATA_SIZE],
            };
            self.inner
                .resolve_link(entry)
                .expect("entry was just generated so it must be valid")
                .data()
                .write(&mut &mut node.inner[..])
                .expect("buffer was created with enough capacity");
            new_nodes.push(node);
        }
        Ok(new_nodes)
    }

    /// Append multiple blocks to the tree.
    fn append_leaf_iter(
        &mut self,
        vals: impl Iterator<Item = (Arc<Block>, sapling::tree::Root)>,
    ) -> Result<Vec<NodeData>, zcash_history::Error> {
        let mut new_nodes = Vec::new();
        for (block, root) in vals {
            new_nodes.append(&mut self.append_leaf(block, &root)?);
        }
        Ok(new_nodes)
    }

    /// Remove the last leaf (block) from the tree.
    ///
    /// Returns the number of nodes removed from the tree after the operation.
    fn truncate_leaf(&mut self) -> Result<u32, zcash_history::Error> {
        self.inner.truncate_leaf()
    }

    /// Return the root hash of the tree, i.e. `hashChainHistoryRoot`.
    fn hash(&self) -> ChainHistoryMmrRootHash {
        // Both append_leaf() and truncate_leaf() leave a root node, so it should
        // always exist.
        self.inner
            .root_node()
            .expect("must have root node")
            .data()
            .hash()
            .into()
    }
}

/// Convert a Block into a zcash_history::NodeData used in the MMR tree.
///
/// `sapling_root` is the root of the Sapling note commitment tree of the block.
fn block_to_history_node(
    block: Arc<Block>,
    network: Network,
    sapling_root: &sapling::tree::Root,
) -> zcash_history::NodeData {
    let height = block
        .coinbase_height()
        .expect("block must have coinbase height during contextual verification");
    let branch_id = ConsensusBranchId::current(network, height)
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
    let work = bigint::U256::from_big_endian(&work.as_u128().to_be_bytes());

    let sapling_tx_count = count_sapling_transactions(block);

    zcash_history::NodeData {
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
    }
}

/// Count how many Sapling transactions exist in a block,
/// i.e. transactions "where either of vSpendsSapling or vOutputsSapling is non-empty"
/// (https://zips.z.cash/zip-0221#tree-node-specification).
fn count_sapling_transactions(block: Arc<Block>) -> u64 {
    block
        .transactions
        .iter()
        .filter(|tx| tx.has_sapling_shielded_data())
        .count()
        .try_into()
        .expect("number of transactions must fit u64")
}

#[cfg(test)]
mod test {
    use crate::{
        block::Commitment::{self, ChainHistoryActivationReserved},
        serialization::ZcashDeserializeInto,
    };

    use super::*;
    use color_eyre::eyre;
    use eyre::Result;
    use zebra_test::vectors::{
        MAINNET_BLOCKS, MAINNET_FINAL_SAPLING_ROOTS, TESTNET_BLOCKS, TESTNET_FINAL_SAPLING_ROOTS,
    };

    /// Test the MMR tree using the activation block of a network upgrade
    /// and its next block.
    #[test]
    fn tree() -> Result<()> {
        tree_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Heartwood)?;
        tree_for_network_upgrade(Network::Testnet, NetworkUpgrade::Heartwood)?;
        tree_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Canopy)?;
        tree_for_network_upgrade(Network::Testnet, NetworkUpgrade::Canopy)?;
        Ok(())
    }

    fn tree_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) -> Result<()> {
        let (blocks, sapling_roots) = match network {
            Network::Mainnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SAPLING_ROOTS),
            Network::Testnet => (&*TESTNET_BLOCKS, &*TESTNET_FINAL_SAPLING_ROOTS),
        };
        let height = network_upgrade.activation_height(network).unwrap().0;

        // Load Block 0 (activation block of the given network upgrade)
        let block0 = Arc::new(
            blocks
                .get(&height)
                .expect("test vector exists")
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid"),
        );

        // Check its commitment
        let commitment0 = block0.commitment(network)?;
        if network_upgrade == NetworkUpgrade::Heartwood {
            // Heartwood is the only upgrade that has a reserved value.
            // (For other upgrades we could compare with the expected commitment,
            // but we haven't calculated them.)
            assert_eq!(commitment0, ChainHistoryActivationReserved);
        }

        // Build initial MMR tree with only Block 0
        let sapling_root0 =
            sapling::tree::Root(**sapling_roots.get(&height).expect("test vector exists"));
        let mut tree = Tree::new_from_block(network, block0, &sapling_root0)?;

        // Compute root hash of the MMR tree, which will be included in the next block
        let hash0 = tree.hash();

        // Load Block 1 (activation + 1)
        let block1 = Arc::new(
            blocks
                .get(&(height + 1))
                .expect("test vector exists")
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid"),
        );

        // Check its commitment
        let commitment1 = block1.commitment(network)?;
        assert_eq!(commitment1, Commitment::ChainHistoryRoot(hash0));

        // Append Block to MMR tree
        let sapling_root1 = sapling::tree::Root(
            **sapling_roots
                .get(&(height + 1))
                .expect("test vector exists"),
        );
        let append = tree.append_leaf(block1, &sapling_root1).unwrap();

        // Tree how has 3 nodes: two leafs for each block, and one parent node
        // which is the new root
        assert_eq!(tree.inner.len(), 3);
        // Two nodes were appended: the new leaf and the parent node
        assert_eq!(append.len(), 2);

        Ok(())
    }
}
