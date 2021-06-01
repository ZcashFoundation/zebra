//! Contains code that interfaces with the zcash_history crate from
//! librustzcash.

use std::{collections::HashMap, convert::TryInto, io};

use crate::{
    block::Block,
    parameters::{ConsensusBranchId, Network},
    sapling,
    transaction::Transaction,
};

/// A MMR Tree using zcash_history::Tree.
///
/// Currently it should not be used as a long-term data structure because it
/// may grow undefinitely.
pub struct Tree {
    network: Network,
    tree: zcash_history::Tree,
}

/// An encoded tree Node.
pub struct Node {
    node: [u8; zcash_history::MAX_NODE_DATA_SIZE],
}

impl Node {
    /// Convert a Node into a zcash_history::Entry.
    fn to_entry(&self, branch_id: ConsensusBranchId) -> Result<zcash_history::Entry, io::Error> {
        zcash_history::Entry::from_bytes(branch_id.into(), self.node)
    }
}

impl Tree {
    /// Create a MMR tree with the given length.
    ///
    /// The `peaks` are the peaks of the MMR tree to build and their position in the
    /// array representation of the tree.
    /// The `extra` are extra nodes that enable removing nodes from the tree, and their position.
    ///
    /// Note that the length is usually larger than the length of `peaks` and `extra`, since
    /// you don't need to pass every node, just the peaks of the tree (plus extra).
    fn new(
        network: Network,
        branch_id: ConsensusBranchId,
        length: u32,
        peaks: &HashMap<u32, &Node>,
        extra: &HashMap<u32, &Node>,
    ) -> Result<Self, io::Error> {
        let mut peaks_vec = Vec::new();
        for (idx, node) in peaks {
            peaks_vec.push((*idx, node.to_entry(branch_id)?));
        }
        let mut extra_vec = Vec::new();
        for (idx, node) in extra {
            extra_vec.push((*idx, node.to_entry(branch_id)?));
        }
        let tree = zcash_history::Tree::new(length, peaks_vec, extra_vec);
        Ok(Tree { network, tree })
    }

    /// Append a new block to the tree, as a new leaf.
    ///
    /// `sapling_root` is the root of the Sapling note commitment tree of the block.
    ///
    /// Returns a vector of nodes added to the tree (leaf + internal nodes).
    fn append_leaf(&mut self, block: Arc<Block>, sapling_root: &sapling::tree::Root) -> Vec<Node> {
        let node_data = convert_block_to_librustzcash_data(block, self.network, sapling_root);
        // TODO: handle error
        let appended = self.tree.append_leaf(node_data).unwrap();

        let mut new_nodes = Vec::new();
        for entry in appended {
            let mut node = Node {
                node: [0; zcash_history::MAX_NODE_DATA_SIZE],
            };
            self.tree
                .resolve_link(entry)
                .expect("entry was just generated so it must be valid")
                .data()
                .write(&mut &mut node.node[..])
                .expect("buffer was created with enough capacity");
            new_nodes.push(node);
        }
        new_nodes
    }

    /// Append multiple blocks to the tree.
    fn append_leaf_iter(
        &mut self,
        vals: impl Iterator<Item = (Arc<Block>, sapling::tree::Root)>,
    ) {
        for (block, root) in vals {
            self.append_leaf(block, root);
        }
    }

    /// Remove the last leaf (block) from the tree.
    ///
    /// Returns the number of nodes removed from the tree after the operation.
    fn truncate_leaf(&mut self) -> u32 {
        // XXX: handle error
        self.tree.truncate_leaf().unwrap()
    }

    /// Return the root hash of the tree, i.e. `hashChainHistoryRoot`.
    fn hash(&self) -> [u8; 32] {
        // Both append_leaf() and truncate_leaf() leave a root node, so it should
        // always exist.
        self.tree
            .root_node()
            .expect("must have root node")
            .data()
            .hash()
    }
}

/// Convert a Block into a zcash_history::NodeData used in the MMR tree.
///
/// `sapling_root` is the root of the Sapling note commitment tree of the block.
fn convert_block_to_librustzcash_data(
    block: &Block,
    network: Network,
    sapling_root: &sapling::tree::Root,
) -> zcash_history::NodeData {
    let height = block
        .coinbase_height()
        .expect("block must have coinbase height during contextual verification");
    let branch_id = ConsensusBranchId::current(network, height).expect("must have branch ID for chain history network upgrades");
    let block_hash = block.hash().0;
    let time: u32 = block
        .header
        .time
        .timestamp()
        .try_into()
        .expect("deserialized and generated timestamps are u32 values");
    let target = block.header.difficulty_threshold.0;
    let sapling_root: [u8; 32] = (*sapling_root).into();
    let work = block
        .header
        .difficulty_threshold
        .to_work()
        .expect("work must be valid during contextual verification");
    let work = work.as_u128().to_be_bytes();

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
        subtree_total_work: (&work[..]).into(),
        start_height: height.0 as u64,
        end_height: height.0 as u64,
        sapling_tx: sapling_tx_count,
    }
}

/// Count how many Sapling transactions exist in a block,
/// i.e. transactions "where either of vSpendsSapling or vOutputsSapling is non-empty"
/// (https://zips.z.cash/zip-0221#tree-node-specification).
fn count_sapling_transactions(block: &Block) -> u64 {
    block
        .transactions
        .iter()
        .map(|tx| -> u64 {
            match &**tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => 0,
                Transaction::V4 {
                    sapling_shielded_data,
                    ..
                } => match sapling_shielded_data {
                    Some(_) => 1,
                    None => 0,
                },
                Transaction::V5 {
                    sapling_shielded_data,
                    ..
                } => match sapling_shielded_data {
                    Some(_) => 1,
                    None => 0,
                },
            }
        })
        .sum()
}
