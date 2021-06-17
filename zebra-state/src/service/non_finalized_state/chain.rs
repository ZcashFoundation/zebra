use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Deref,
};

use tracing::{debug_span, instrument, trace};
use zebra_chain::{
    block::{self, ChainHistoryMmrRootHash},
    mmr::HistoryTree,
    orchard,
    primitives::Groth16Proof,
    sapling, sprout, transaction,
    transaction::Transaction::*,
    transparent,
    work::difficulty::PartialCumulativeWork,
};

use crate::{PreparedBlock, Utxo};

// #[derive(Clone)]
pub struct Chain {
    pub blocks: BTreeMap<block::Height, PreparedBlock>,
    pub height_by_hash: HashMap<block::Hash, block::Height>,
    pub tx_by_hash: HashMap<transaction::Hash, (block::Height, usize)>,

    pub created_utxos: HashMap<transparent::OutPoint, Utxo>,
    spent_utxos: HashSet<transparent::OutPoint>,
    // TODO: add sprout, sapling and orchard anchors (#1320)
    sprout_anchors: HashSet<sprout::tree::Root>,
    sapling_anchors: HashSet<sapling::tree::Root>,
    sprout_nullifiers: HashSet<sprout::Nullifier>,
    sapling_nullifiers: HashSet<sapling::Nullifier>,
    orchard_nullifiers: HashSet<orchard::Nullifier>,
    partial_cumulative_work: PartialCumulativeWork,
    history_tree: HistoryTree,
}

impl Chain {
    /// Create a new empty non-finalized chain with the given history tree.
    ///
    /// The history tree must contain the history of the previous (finalized) blocks.
    pub fn new(history_tree: HistoryTree) -> Self {
        Chain {
            blocks: Default::default(),
            height_by_hash: Default::default(),
            tx_by_hash: Default::default(),
            created_utxos: Default::default(),
            spent_utxos: Default::default(),
            sprout_anchors: Default::default(),
            sapling_anchors: Default::default(),
            sprout_nullifiers: Default::default(),
            sapling_nullifiers: Default::default(),
            orchard_nullifiers: Default::default(),
            partial_cumulative_work: Default::default(),
            history_tree,
        }
    }

    /// Push a contextually valid non-finalized block into a chain as the new tip.
    #[instrument(level = "debug", skip(self, block), fields(block = %block.block))]
    pub fn push(&mut self, block: PreparedBlock) {
        // update cumulative data members
        self.update_chain_state_with(&block);
        tracing::debug!(block = %block.block, "adding block to chain");
        self.blocks.insert(block.height, block);
    }

    /// Remove the lowest height block of the non-finalized portion of a chain.
    #[instrument(level = "debug", skip(self))]
    pub fn pop_root(&mut self) -> PreparedBlock {
        let block_height = self.lowest_height();

        // remove the lowest height block from self.blocks
        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        // update cumulative data members
        self.revert_chain_state_with(&block);

        // return the prepared block
        block
    }

    fn lowest_height(&self) -> block::Height {
        self.blocks
            .keys()
            .next()
            .cloned()
            .expect("only called while blocks is populated")
    }

    /// Fork a chain at the block with the given hash, if it is part of this
    /// chain.
    ///
    /// `finalized_tip_history_tree`: the history tree for the finalized tip
    /// from which the tree of the fork will be computed.
    pub fn fork(
        &self,
        fork_tip: block::Hash,
        finalized_tip_history_tree: &HistoryTree,
    ) -> Option<Self> {
        if !self.height_by_hash.contains_key(&fork_tip) {
            return None;
        }

        let mut forked = self.with_history_tree(finalized_tip_history_tree.clone());

        while forked.non_finalized_tip_hash() != fork_tip {
            forked.pop_tip();
        }

        // Rebuild the history tree starting from the finalized tip tree.
        // TODO: change to a more efficient approach by removing nodes
        // from the tree of the original chain (in `pop_tip()`).
        forked.history_tree.extend(
            forked
                .blocks
                .values()
                .map(|prepared_block| prepared_block.block.clone()),
        );

        Some(forked)
    }

    pub fn non_finalized_tip_hash(&self) -> block::Hash {
        self.blocks
            .values()
            .next_back()
            .expect("only called while blocks is populated")
            .hash
    }

    /// Remove the highest height block of the non-finalized portion of a chain.
    fn pop_tip(&mut self) {
        let block_height = self.non_finalized_tip_height();

        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        assert!(
            !self.blocks.is_empty(),
            "Non-finalized chains must have at least one block to be valid"
        );

        self.revert_chain_state_with(&block);
    }

    pub fn non_finalized_tip_height(&self) -> block::Height {
        *self
            .blocks
            .keys()
            .next_back()
            .expect("only called while blocks is populated")
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn history_root_hash(&self) -> ChainHistoryMmrRootHash {
        self.history_tree.hash()
    }

    /// Clone the Chain but not the history tree, using the history tree
    /// specified instead.
    ///
    /// Useful when forking, where the history tree is rebuilt anyway.
    fn with_history_tree(&self, history_tree: HistoryTree) -> Self {
        Chain {
            blocks: self.blocks.clone(),
            height_by_hash: self.height_by_hash.clone(),
            tx_by_hash: self.tx_by_hash.clone(),
            created_utxos: self.created_utxos.clone(),
            spent_utxos: self.spent_utxos.clone(),
            sprout_anchors: self.sprout_anchors.clone(),
            sapling_anchors: self.sapling_anchors.clone(),
            sprout_nullifiers: self.sprout_nullifiers.clone(),
            sapling_nullifiers: self.sapling_nullifiers.clone(),
            orchard_nullifiers: self.orchard_nullifiers.clone(),
            partial_cumulative_work: self.partial_cumulative_work,
            history_tree,
        }
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

impl UpdateWith<PreparedBlock> for Chain {
    fn update_chain_state_with(&mut self, prepared: &PreparedBlock) {
        let (block, hash, height, transaction_hashes) = (
            prepared.block.as_ref(),
            prepared.hash,
            prepared.height,
            &prepared.transaction_hashes,
        );

        // add hash to height_by_hash
        let prior_height = self.height_by_hash.insert(hash, height);
        assert!(
            prior_height.is_none(),
            "block heights must be unique within a single chain"
        );

        // add work to partial cumulative work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work += block_work;

        self.history_tree.push(block);

        // for each transaction in block
        for (transaction_index, (transaction, transaction_hash)) in block
            .transactions
            .iter()
            .zip(transaction_hashes.iter().cloned())
            .enumerate()
        {
            let (
                inputs,
                joinsplit_data,
                sapling_shielded_data_per_spend_anchor,
                sapling_shielded_data_shared_anchor,
                orchard_shielded_data,
            ) = match transaction.deref() {
                V4 {
                    inputs,
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => (inputs, joinsplit_data, sapling_shielded_data, &None, &None),
                V5 {
                    inputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),
                V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(
                    "older transaction versions only exist in finalized blocks, because of the mandatory canopy checkpoint",
                ),
            };

            // add key `transaction.hash` and value `(height, tx_index)` to `tx_by_hash`
            let prior_pair = self
                .tx_by_hash
                .insert(transaction_hash, (height, transaction_index));
            assert!(
                prior_pair.is_none(),
                "transactions must be unique within a single chain"
            );

            // add the utxos this produced
            self.update_chain_state_with(&prepared.new_outputs);
            // add the utxos this consumed
            self.update_chain_state_with(inputs);

            // add the shielded data
            self.update_chain_state_with(joinsplit_data);
            self.update_chain_state_with(sapling_shielded_data_per_spend_anchor);
            self.update_chain_state_with(sapling_shielded_data_shared_anchor);
            self.update_chain_state_with(orchard_shielded_data);
        }
    }

    #[instrument(skip(self, prepared), fields(block = %prepared.block))]
    fn revert_chain_state_with(&mut self, prepared: &PreparedBlock) {
        let (block, hash, transaction_hashes) = (
            prepared.block.as_ref(),
            prepared.hash,
            &prepared.transaction_hashes,
        );

        // remove the blocks hash from `height_by_hash`
        assert!(
            self.height_by_hash.remove(&hash).is_some(),
            "hash must be present if block was"
        );

        // remove work from partial_cumulative_work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work -= block_work;

        // Note: the MMR is not reverted here.
        // When popping the root: there is no need to change the MMR.
        // When popping the tip: the MMR is rebuilt in fork().

        // for each transaction in block
        for (transaction, transaction_hash) in
            block.transactions.iter().zip(transaction_hashes.iter())
        {
            let (
                inputs,
                joinsplit_data,
                sapling_shielded_data_per_spend_anchor,
                sapling_shielded_data_shared_anchor,
                orchard_shielded_data,
            ) = match transaction.deref() {
                V4 {
                    inputs,
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => (inputs, joinsplit_data, sapling_shielded_data, &None, &None),
                V5 {
                    inputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),
                V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(
                    "older transaction versions only exist in finalized blocks, because of the mandatory canopy checkpoint",
                ),
            };

            // remove `transaction.hash` from `tx_by_hash`
            assert!(
                self.tx_by_hash.remove(transaction_hash).is_some(),
                "transactions must be present if block was"
            );

            // remove the utxos this produced
            self.revert_chain_state_with(&prepared.new_outputs);
            // remove the utxos this consumed
            self.revert_chain_state_with(inputs);

            // remove the shielded data
            self.revert_chain_state_with(joinsplit_data);
            self.revert_chain_state_with(sapling_shielded_data_per_spend_anchor);
            self.revert_chain_state_with(sapling_shielded_data_shared_anchor);
            self.revert_chain_state_with(orchard_shielded_data);
        }
    }
}

impl UpdateWith<HashMap<transparent::OutPoint, Utxo>> for Chain {
    fn update_chain_state_with(&mut self, utxos: &HashMap<transparent::OutPoint, Utxo>) {
        self.created_utxos
            .extend(utxos.iter().map(|(k, v)| (*k, v.clone())));
    }

    fn revert_chain_state_with(&mut self, utxos: &HashMap<transparent::OutPoint, Utxo>) {
        self.created_utxos
            .retain(|outpoint, _| !utxos.contains_key(outpoint));
    }
}

impl UpdateWith<Vec<transparent::Input>> for Chain {
    fn update_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    self.spent_utxos.insert(*outpoint);
                }
                transparent::Input::Coinbase { .. } => {}
            }
        }
    }

    fn revert_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    assert!(
                        self.spent_utxos.remove(outpoint),
                        "spent_utxos must be present if block was"
                    );
                }
                transparent::Input::Coinbase { .. } => {}
            }
        }
    }
}

impl UpdateWith<Option<transaction::JoinSplitData<Groth16Proof>>> for Chain {
    #[instrument(skip(self, joinsplit_data))]
    fn update_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit { nullifiers, .. } in joinsplit_data.joinsplits() {
                let span = debug_span!("revert_chain_state_with", ?nullifiers);
                let _entered = span.enter();
                trace!("Adding sprout nullifiers.");
                self.sprout_nullifiers.insert(nullifiers[0]);
                self.sprout_nullifiers.insert(nullifiers[1]);
            }
        }
    }

    #[instrument(skip(self, joinsplit_data))]
    fn revert_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit { nullifiers, .. } in joinsplit_data.joinsplits() {
                let span = debug_span!("revert_chain_state_with", ?nullifiers);
                let _entered = span.enter();
                trace!("Removing sprout nullifiers.");
                assert!(
                    self.sprout_nullifiers.remove(&nullifiers[0]),
                    "nullifiers must be present if block was"
                );
                assert!(
                    self.sprout_nullifiers.remove(&nullifiers[1]),
                    "nullifiers must be present if block was"
                );
            }
        }
    }
}

impl<AnchorV> UpdateWith<Option<sapling::ShieldedData<AnchorV>>> for Chain
where
    AnchorV: sapling::AnchorVariant + Clone,
{
    fn update_chain_state_with(
        &mut self,
        sapling_shielded_data: &Option<sapling::ShieldedData<AnchorV>>,
    ) {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            for nullifier in sapling_shielded_data.nullifiers() {
                self.sapling_nullifiers.insert(*nullifier);
            }
        }
    }

    fn revert_chain_state_with(
        &mut self,
        sapling_shielded_data: &Option<sapling::ShieldedData<AnchorV>>,
    ) {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            for nullifier in sapling_shielded_data.nullifiers() {
                assert!(
                    self.sapling_nullifiers.remove(nullifier),
                    "nullifier must be present if block was"
                );
            }
        }
    }
}

impl UpdateWith<Option<orchard::ShieldedData>> for Chain {
    fn update_chain_state_with(&mut self, orchard_shielded_data: &Option<orchard::ShieldedData>) {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            for nullifier in orchard_shielded_data.nullifiers() {
                self.orchard_nullifiers.insert(*nullifier);
            }
        }
    }

    fn revert_chain_state_with(&mut self, orchard_shielded_data: &Option<orchard::ShieldedData>) {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            for nullifier in orchard_shielded_data.nullifiers() {
                assert!(
                    self.orchard_nullifiers.remove(nullifier),
                    "nullifier must be present if block was"
                );
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
        Some(self.cmp(other))
    }
}

impl Ord for Chain {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            self.partial_cumulative_work
                .cmp(&other.partial_cumulative_work)
        } else {
            let self_hash = self
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash;

            let other_hash = other
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash;

            // This comparison is a tie-breaker within the local node, so it does not need to
            // be consistent with the ordering on `ExpandedDifficulty` and `block::Hash`.
            match self_hash.0.cmp(&other_hash.0) {
                Ordering::Equal => unreachable!("Chain tip block hashes are always unique"),
                ordering => ordering,
            }
        }
    }
}
