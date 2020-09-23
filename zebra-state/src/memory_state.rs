use std::{
    cmp::Ordering,
    collections::BTreeSet,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Deref,
    sync::Arc,
};

use zebra_chain::{
    block::{self, Block},
    primitives::Groth16Proof,
    sapling, sprout, transaction, transparent,
    work::difficulty::PartialCumulativeWork,
    work::difficulty::Work,
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

#[derive(Debug, Default, Clone)]
struct Chain {
    blocks: BTreeMap<block::Height, Arc<Block>>,
    height_by_hash: HashMap<block::Hash, block::Height>,
    tx_by_hash: HashMap<transaction::Hash, (block::Height, TodoSize)>,

    utxos: HashSet<transparent::Output>,
    sprout_anchors: HashSet<sprout::tree::Root>,
    sapling_anchors: HashSet<sapling::tree::Root>,
    sprout_nullifiers: HashSet<sprout::Nullifier>,
    sapling_nullifiers: HashSet<sapling::Nullifier>,
    partial_cumulative_work: PartialCumulativeWork,
}

impl Chain {
    pub fn push(&mut self, block: Arc<Block>) {
        let block_height = block
            .coinbase_height()
            .expect("valid non-finalized blocks have a coinbase height");

        self.update_chain_state_with(&block);

        self.blocks.insert(block_height, block);
    }

    pub fn pop_root(&mut self) -> Arc<Block> {
        let block_height = self.lowest_height();

        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while block is populated");

        self.revert_chain_state_with(&block);

        block
    }

    fn lowest_height(&self) -> block::Height {
        self.blocks
            .keys()
            .next()
            .cloned()
            .expect("only called while block is populated")
    }

    pub fn fork(&self, new_tip: block::Hash) -> Option<Self> {
        if !self.height_by_hash.contains_key(&new_tip) {
            return None;
        }

        let mut forked = self.clone();

        while forked.non_finalized_tip_hash() != new_tip {
            forked.pop_tip();
        }

        Some(forked)
    }

    fn non_finalized_tip_hash(&self) -> block::Hash {
        self.blocks
            .values()
            .next_back()
            .expect("only called while block is populated")
            .hash()
    }

    fn pop_tip(&mut self) {
        let block_height = self.non_finalized_tip_height();

        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while block is populated");

        self.revert_chain_state_with(&block);
    }

    fn non_finalized_tip_height(&self) -> block::Height {
        *self
            .blocks
            .keys()
            .next_back()
            .expect("only called while block is populated")
    }
}

/// Helper trait to organize inverse operations done on the `Chain` type. Used to
/// overload the `update_chain_state_with` and `revert_chain_state_with` methods
/// based on the type of the argument.
///
/// This trait was motivated by the length of the `push` and `pop_root` functions
/// and fear that it would be easy to introduce bugs when updating them unless
/// the code was reorganized to keep related operations adjacent to eachother.
trait UpdateWith<T> {
    /// Update `Chain` cumulative data members to add data that are derived from
    /// `T`
    fn update_chain_state_with(&mut self, _: &T);

    /// Update `Chain` cumulative data members to remove data that are derived
    /// from `T`
    fn revert_chain_state_with(&mut self, _: &T);
}

impl UpdateWith<Arc<Block>> for Chain {
    fn update_chain_state_with(&mut self, block: &Arc<Block>) {
        let block_height = block
            .coinbase_height()
            .expect("valid non-finalized blocks have a coinbase height");

        let block_hash = block.hash();

        let prior_height = self.height_by_hash.insert(block_hash, block_height);
        assert!(prior_height.is_none());

        for (transaction_index, transaction) in block.transactions.iter().enumerate() {
            let (inputs, outputs, shielded_data, joinsplit_data) = match transaction.deref() {
                transaction::Transaction::V4 {
                    inputs,
                    outputs,
                    shielded_data,
                    joinsplit_data,
                    ..
                } => (inputs, outputs, shielded_data, joinsplit_data),
                _ => unreachable!(
                    "older transaction versions only exist in finalized blocks pre sapling",
                ),
            };

            let transaction_hash = transaction.hash();
            let prior_pair = self
                .tx_by_hash
                .insert(transaction_hash, (block_height, transaction_index));

            assert!(prior_pair.is_none());

            // add deltas for utxos this produced
            self.update_chain_state_with(outputs);
            // add deltas for utxos this consumed
            self.update_chain_state_with(inputs);
            // add sprout anchor and nullifiers
            self.update_chain_state_with(joinsplit_data);
            // add sapling anchor and nullifier
            self.update_chain_state_with(shielded_data);
        }

        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("XXX: explain why we should always have work");

        self.partial_cumulative_work += block_work;
    }

    fn revert_chain_state_with(&mut self, block: &Arc<Block>) {
        let block_hash = block.hash();

        assert!(self.height_by_hash.remove(&block_hash).is_some());

        for (transaction_index, transaction) in block.transactions.iter().enumerate() {
            let (inputs, outputs, shielded_data, joinsplit_data) = match transaction.deref() {
                transaction::Transaction::V4 {
                    inputs,
                    outputs,
                    shielded_data,
                    joinsplit_data,
                    ..
                } => (inputs, outputs, shielded_data, joinsplit_data),
                _ => unreachable!(
                    "older transaction versions only exist in finalized blocks pre sapling",
                ),
            };

            let transaction_hash = transaction.hash();

            assert!(self.tx_by_hash.remove(&transaction_hash).is_some());

            // remove the deltas for utxos this produced
            self.revert_chain_state_with(outputs);
            // remove the deltas for utxos this consumed
            self.revert_chain_state_with(inputs);
            // remove sprout anchor and nullifiers
            self.revert_chain_state_with(joinsplit_data);
            // remove sapling anchor and nullfier
            self.revert_chain_state_with(shielded_data);
        }

        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("XXX: explain why we should always have work");

        self.partial_cumulative_work -= block_work;
    }
}

impl UpdateWith<Vec<transparent::Output>> for Chain {
    fn update_chain_state_with(&mut self, outputs: &Vec<transparent::Output>) {
        for created_utxo in outputs {
            self.utxos.insert(created_utxo.clone());
        }
    }

    fn revert_chain_state_with(&mut self, outputs: &Vec<transparent::Output>) {
        for created_utxo in outputs {
            assert!(self.utxos.remove(&created_utxo));
        }
    }
}

impl UpdateWith<Vec<transparent::Input>> for Chain {
    fn update_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut {
                    outpoint: transparent::OutPoint { hash, index },
                    unlock_script,
                    sequence,
                } => todo!(
                    "we need to change the representation of this at because
                    they may remove finalized utxos but we do not wish for
                    that removal itself to be finalized"
                ),
                transparent::Input::Coinbase {
                    height,
                    data,
                    sequence,
                } => todo!("what do we do with these?"),
            }
        }
    }

    fn revert_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut {
                    outpoint: transparent::OutPoint { hash, index },
                    unlock_script,
                    sequence,
                } => todo!(
                    "we need to change the representation of this at because
                    they may remove finalized utxos but we do not wish for
                    that removal itself to be finalized"
                ),
                transparent::Input::Coinbase {
                    height,
                    data,
                    sequence,
                } => todo!("what do we do with these?"),
            }
        }
    }
}

impl UpdateWith<Option<transaction::JoinSplitData<Groth16Proof>>> for Chain {
    fn update_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit {
                anchor, nullifiers, ..
            } in joinsplit_data.joinsplits()
            {
                self.sprout_anchors.insert(*anchor);
                self.sprout_nullifiers.insert(nullifiers[0]);
                self.sprout_nullifiers.insert(nullifiers[1]);
            }
        }
    }

    fn revert_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit {
                anchor, nullifiers, ..
            } in joinsplit_data.joinsplits()
            {
                assert!(self.sprout_anchors.remove(anchor));
                assert!(self.sprout_nullifiers.remove(&nullifiers[0]));
                assert!(self.sprout_nullifiers.remove(&nullifiers[1]));
            }
        }
    }
}

impl UpdateWith<Option<transaction::ShieldedData>> for Chain {
    fn update_chain_state_with(&mut self, shielded_data: &Option<transaction::ShieldedData>) {
        if let Some(shielded_data) = shielded_data {
            for sapling::Spend {
                anchor, nullifier, ..
            } in shielded_data.spends()
            {
                self.sapling_anchors.insert(*anchor);
                self.sapling_nullifiers.insert(*nullifier);
            }
        }
    }

    fn revert_chain_state_with(&mut self, shielded_data: &Option<transaction::ShieldedData>) {
        if let Some(shielded_data) = shielded_data {
            for sapling::Spend {
                anchor, nullifier, ..
            } in shielded_data.spends()
            {
                assert!(self.sapling_anchors.remove(anchor));
                assert!(self.sapling_nullifiers.remove(nullifier));
            }
        }
    }
}

impl PartialEq for Chain {
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl Eq for Chain {}

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            self.partial_cumulative_work
                .partial_cmp(&other.partial_cumulative_work)
        } else {
            None
        }
    }
}

impl Ord for Chain {
    fn cmp(&self, other: &Self) -> Ordering {
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
