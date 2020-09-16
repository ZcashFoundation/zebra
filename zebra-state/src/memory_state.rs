use std::{
    collections::BTreeSet,
    collections::HashSet,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use zebra_chain::{
    block::{self, Block},
    sapling, sprout, transaction, transparent,
};

use crate::{service::QueuedBlock, BoxError};

// Todo replace usages of this with the correct integer types
type TodoSize = usize;

pub struct MemoryState {
    // TODO
}

struct ChainSet {
    chains: BTreeSet<Chain>,

    queued_blocks: BTreeMap<block::Hash, QueuedBlock>,
    queued_by_parent: BTreeMap<block::Hash, Vec<block::Hash>>,
    queued_by_height: BTreeMap<block::Height, Vec<block::Hash>>,
}

impl ChainSet {
    pub fn finalize(&mut self) -> Arc<Block> {
        todo!()
    }

    pub fn queue(&mut self, block: QueuedBlock) {
        todo!()
    }

    fn process_queued(&mut self, new_parent: block::Hash) {
        todo!()
    }

    fn commit_block(&mut self, block: QueuedBlock) -> Option<block::Hash> {
        todo!()
    }
}

struct Chain {
    blocks: BTreeMap<block::Height, Arc<Block>>,
    height_by_hash: HashMap<block::Hash, block::Height>,
    tx_by_hash: HashMap<transaction::Hash, (block::Height, TodoSize)>,

    utxos: HashSet<transparent::Output>,
    sapling_anchors: HashSet<sapling::tree::Root>,
    sprout_anchors: HashSet<sprout::tree::Root>,
    sapling_nullifiers: HashSet<sapling::Nullifier>,
    sprout_nullifiers: HashSet<sprout::Nullifier>,
    partial_cumulative_work: PartialCumulativeWork,
}

impl Chain {
    pub fn push(&mut self, block: Arc<Block>) -> Result<(), BoxError> {
        todo!()
    }

    pub fn pop_root(&mut self) -> Arc<Block> {
        todo!()
    }

    pub fn fork(&self, new_typ: block::Hash) -> Option<Self> {
        todo!()
    }

    fn pop_tip(&mut self) -> Arc<Block> {
        todo!()
    }
}

impl PartialEq for Chain {
    fn eq(&self, other: &Self) -> bool {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            return false;
        }

        self.blocks.eq(&other.blocks)
    }
}

impl Eq for Chain {}

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            self.partial_cumulative_work
                .partial_cmp(&other.partial_cumulative_work)
        } else {
            None
        }
    }
}

impl Ord for Chain {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other)
            .or_else(|| {
                let self_hash = self
                    .blocks
                    .values()
                    .last()
                    .expect("always at least 1 element")
                    .hash();

                let other_hash = other
                    .blocks
                    .values()
                    .last()
                    .expect("always at least 1 element")
                    .hash();

                self_hash.0.partial_cmp(&other_hash.0)
            })
            .expect("block hashes are always unique")
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct PartialCumulativeWork(TodoSize);
