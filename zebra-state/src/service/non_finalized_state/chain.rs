use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Deref,
};

use multiset::HashMultiSet;
use tracing::instrument;

use zebra_chain::{
    block, orchard, primitives::Groth16Proof, sapling, sprout, transaction,
    transaction::Transaction::*, transparent, work::difficulty::PartialCumulativeWork,
};

use crate::{service::check, ContextuallyValidBlock, PreparedBlock, ValidateContextError};

#[derive(Debug, Clone)]
pub struct Chain {
    /// The contextually valid blocks which form this non-finalized partial chain, in height order.
    pub(crate) blocks: BTreeMap<block::Height, ContextuallyValidBlock>,

    /// An index of block heights for each block hash in `blocks`.
    pub height_by_hash: HashMap<block::Hash, block::Height>,
    /// An index of block heights and transaction indexes for each transaction hash in `blocks`.
    pub tx_by_hash: HashMap<transaction::Hash, (block::Height, usize)>,

    /// The [`Utxo`]s created by `blocks`.
    ///
    /// Note that these UTXOs may not be unspent.
    /// Outputs can be spent by later transactions or blocks in the chain.
    pub(crate) created_utxos: HashMap<transparent::OutPoint, transparent::Utxo>,
    /// The [`OutPoint`]s spent by `blocks`,
    /// including those created by earlier transactions or blocks in the chain.
    pub(crate) spent_utxos: HashSet<transparent::OutPoint>,

    /// The Sprout note commitment tree of the tip of this Chain.
    pub(super) sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
    /// The Sapling note commitment tree of the tip of this Chain.
    pub(super) sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
    /// The Orchard note commitment tree of the tip of this Chain.
    pub(super) orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,

    /// The Sprout anchors created by `blocks`.
    /// This does this include interstitial anchors.
    pub(super) sprout_anchors: HashMultiSet<sprout::tree::Root>,
    /// The Sprout anchors created by each block in the chain.
    pub(super) sprout_anchors_by_height: BTreeMap<block::Height, sprout::tree::Root>,
    /// The Sapling anchors created by `blocks`.
    pub(super) sapling_anchors: HashMultiSet<sapling::tree::Root>,
    /// The Sapling anchors created by each block in the chain.
    pub(super) sapling_anchors_by_height: BTreeMap<block::Height, sapling::tree::Root>,
    /// The Orchard anchors created by `blocks`.
    pub(super) orchard_anchors: HashMultiSet<orchard::tree::Root>,
    /// The Orchard anchors created by each block in the chain.
    pub(super) orchard_anchors_by_height: BTreeMap<block::Height, orchard::tree::Root>,

    /// The Sprout nullifiers revealed by `blocks`.
    pub(super) sprout_nullifiers: HashSet<sprout::Nullifier>,
    /// The Sapling nullifiers revealed by `blocks`.
    pub(super) sapling_nullifiers: HashSet<sapling::Nullifier>,
    /// The Orchard nullifiers revealed by `blocks`.
    pub(super) orchard_nullifiers: HashSet<orchard::Nullifier>,

    /// The cumulative work represented by this partial non-finalized chain.
    pub(super) partial_cumulative_work: PartialCumulativeWork,
}

impl Chain {
    // Create a new Chain with the given note commitment trees.
    pub(crate) fn new(
        sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
    ) -> Self {
        Self {
            blocks: Default::default(),
            height_by_hash: Default::default(),
            tx_by_hash: Default::default(),
            created_utxos: Default::default(),
            sprout_note_commitment_tree,
            sapling_note_commitment_tree,
            orchard_note_commitment_tree,
            spent_utxos: Default::default(),
            sprout_anchors: HashMultiSet::new(),
            sprout_anchors_by_height: Default::default(),
            sapling_anchors: HashMultiSet::new(),
            sapling_anchors_by_height: Default::default(),
            orchard_anchors: HashMultiSet::new(),
            orchard_anchors_by_height: Default::default(),
            sprout_nullifiers: Default::default(),
            sapling_nullifiers: Default::default(),
            orchard_nullifiers: Default::default(),
            partial_cumulative_work: Default::default(),
        }
    }

    /// Is the internal state of `self` the same as `other`?
    ///
    /// [`Chain`] has custom [`Eq`] and [`Ord`] implementations based on proof of work,
    /// which are used to select the best chain. So we can't derive [`Eq`] for [`Chain`].
    ///
    /// Unlike the custom trait impls, this method returns `true` if the entire internal state
    /// of two chains is equal.
    ///
    /// If the internal states are different, it returns `false`,
    /// even if the blocks in the two chains are equal.
    #[cfg(test)]
    pub(crate) fn eq_internal_state(&self, other: &Chain) -> bool {
        // this method must be updated every time a field is added to Chain

        // blocks, heights, hashes
        self.blocks == other.blocks &&
            self.height_by_hash == other.height_by_hash &&
            self.tx_by_hash == other.tx_by_hash &&

            // transparent UTXOs
            self.created_utxos == other.created_utxos &&
            self.spent_utxos == other.spent_utxos &&

            // note commitment trees
            self.sprout_note_commitment_tree.root() == other.sprout_note_commitment_tree.root() &&
            self.sapling_note_commitment_tree.root() == other.sapling_note_commitment_tree.root() &&
            self.orchard_note_commitment_tree.root() == other.orchard_note_commitment_tree.root() &&

            // anchors
            self.sprout_anchors == other.sprout_anchors &&
            self.sprout_anchors_by_height == other.sprout_anchors_by_height &&
            self.sapling_anchors == other.sapling_anchors &&
            self.sapling_anchors_by_height == other.sapling_anchors_by_height &&
            self.orchard_anchors == other.orchard_anchors &&
            self.orchard_anchors_by_height == other.orchard_anchors_by_height &&

            // nullifiers
            self.sprout_nullifiers == other.sprout_nullifiers &&
            self.sapling_nullifiers == other.sapling_nullifiers &&
            self.orchard_nullifiers == other.orchard_nullifiers &&

            // proof of work
            self.partial_cumulative_work == other.partial_cumulative_work
    }

    /// Push a contextually valid non-finalized block into this chain as the new tip.
    ///
    /// If the block is invalid, drop this chain and return an error.
    #[instrument(level = "debug", skip(self, block), fields(block = %block.block))]
    pub fn push(mut self, block: PreparedBlock) -> Result<Chain, ValidateContextError> {
        // the block isn't contextually valid until `update_chain_state_with` returns success
        let block = ContextuallyValidBlock::from(block);

        // update cumulative data members
        self.update_chain_state_with(&block)?;
        tracing::debug!(block = %block.block, "adding block to chain");
        self.blocks.insert(block.height, block);
        Ok(self)
    }

    /// Remove the lowest height block of the non-finalized portion of a chain.
    #[instrument(level = "debug", skip(self))]
    pub(crate) fn pop_root(&mut self) -> ContextuallyValidBlock {
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
    /// The note commitment trees must be the trees of the finalized tip.
    pub fn fork(
        &self,
        fork_tip: block::Hash,
        sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
    ) -> Result<Option<Self>, ValidateContextError> {
        if !self.height_by_hash.contains_key(&fork_tip) {
            return Ok(None);
        }

        let mut forked = self.with_trees(
            sprout_note_commitment_tree,
            sapling_note_commitment_tree,
            orchard_note_commitment_tree,
        );

        while forked.non_finalized_tip_hash() != fork_tip {
            forked.pop_tip();
        }

        // Rebuild the note commitment trees, starting from the finalized tip tree.
        // TODO: change to a more efficient approach by removing nodes
        // from the tree of the original chain (in `pop_tip()`).
        // See https://github.com/ZcashFoundation/zebra/issues/2378
        for block in forked.blocks.values() {
            for transaction in block.block.transactions.iter() {
                for sprout_note_commitment in transaction.sprout_note_commitments() {
                    forked
                        .sprout_note_commitment_tree
                        .append(sprout_note_commitment);
                }
                for sapling_note_commitment in transaction.sapling_note_commitments() {
                    forked
                        .sapling_note_commitment_tree
                        .append(*sapling_note_commitment)
                        .expect("must work since it was already appended before the fork");
                }
                for orchard_note_commitment in transaction.orchard_note_commitments() {
                    forked
                        .orchard_note_commitment_tree
                        .append(*orchard_note_commitment)
                        .expect("must work since it was already appended before the fork");
                }
            }
        }

        Ok(Some(forked))
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

    /// Returns the unspent transaction outputs (UTXOs) in this non-finalized chain.
    ///
    /// Callers should also check the finalized state for available UTXOs.
    /// If UTXOs remain unspent when a block is finalized, they are stored in the finalized state,
    /// and removed from the relevant chain(s).
    pub fn unspent_utxos(&self) -> HashMap<transparent::OutPoint, transparent::Utxo> {
        let mut unspent_utxos = self.created_utxos.clone();
        unspent_utxos.retain(|out_point, _utxo| !self.spent_utxos.contains(out_point));
        unspent_utxos
    }

    /// Clone the Chain but not the history and note commitment trees, using
    /// the specified trees instead.
    ///
    /// Useful when forking, where the trees are rebuilt anyway.
    fn with_trees(
        &self,
        sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
    ) -> Self {
        Chain {
            blocks: self.blocks.clone(),
            height_by_hash: self.height_by_hash.clone(),
            tx_by_hash: self.tx_by_hash.clone(),
            created_utxos: self.created_utxos.clone(),
            spent_utxos: self.spent_utxos.clone(),
            sprout_note_commitment_tree,
            sapling_note_commitment_tree,
            orchard_note_commitment_tree,
            sprout_anchors: self.sprout_anchors.clone(),
            sapling_anchors: self.sapling_anchors.clone(),
            orchard_anchors: self.orchard_anchors.clone(),
            sprout_anchors_by_height: self.sprout_anchors_by_height.clone(),
            sapling_anchors_by_height: self.sapling_anchors_by_height.clone(),
            orchard_anchors_by_height: self.orchard_anchors_by_height.clone(),
            sprout_nullifiers: self.sprout_nullifiers.clone(),
            sapling_nullifiers: self.sapling_nullifiers.clone(),
            orchard_nullifiers: self.orchard_nullifiers.clone(),
            partial_cumulative_work: self.partial_cumulative_work,
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
    fn update_chain_state_with(&mut self, _: &T) -> Result<(), ValidateContextError>;

    /// Update `Chain` cumulative data members to remove data that are derived
    /// from `T`
    fn revert_chain_state_with(&mut self, _: &T);
}

impl UpdateWith<ContextuallyValidBlock> for Chain {
    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    fn update_chain_state_with(
        &mut self,
        contextually_valid: &ContextuallyValidBlock,
    ) -> Result<(), ValidateContextError> {
        let (block, hash, height, new_outputs, transaction_hashes) = (
            contextually_valid.block.as_ref(),
            contextually_valid.hash,
            contextually_valid.height,
            &contextually_valid.new_outputs,
            &contextually_valid.transaction_hashes,
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
            self.update_chain_state_with(new_outputs)?;
            // add the utxos this consumed
            self.update_chain_state_with(inputs)?;

            // add the shielded data
            self.update_chain_state_with(joinsplit_data)?;
            self.update_chain_state_with(sapling_shielded_data_per_spend_anchor)?;
            self.update_chain_state_with(sapling_shielded_data_shared_anchor)?;
            self.update_chain_state_with(orchard_shielded_data)?;
        }

        // Having updated all the note commitment trees and nullifier sets in
        // this block, the roots of the note commitment trees as of the last
        // transaction are the treestates of this block.
        let root = self.sprout_note_commitment_tree.root();
        self.sprout_anchors.insert(root);
        self.sprout_anchors_by_height.insert(height, root);
        let root = self.sapling_note_commitment_tree.root();
        self.sapling_anchors.insert(root);
        self.sapling_anchors_by_height.insert(height, root);
        let root = self.orchard_note_commitment_tree.root();
        self.orchard_anchors.insert(root);
        self.orchard_anchors_by_height.insert(height, root);

        Ok(())
    }

    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    fn revert_chain_state_with(&mut self, contextually_valid: &ContextuallyValidBlock) {
        let (block, hash, height, new_outputs, transaction_hashes) = (
            contextually_valid.block.as_ref(),
            contextually_valid.hash,
            contextually_valid.height,
            &contextually_valid.new_outputs,
            &contextually_valid.transaction_hashes,
        );

        // remove the blocks hash from `height_by_hash`
        assert!(
            self.height_by_hash.remove(&hash).is_some(),
            "hash must be present if block was added to chain"
        );

        // remove work from partial_cumulative_work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work -= block_work;

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
                "transactions must be present if block was added to chain"
            );

            // remove the utxos this produced
            self.revert_chain_state_with(new_outputs);
            // remove the utxos this consumed
            self.revert_chain_state_with(inputs);

            // remove the shielded data
            self.revert_chain_state_with(joinsplit_data);
            self.revert_chain_state_with(sapling_shielded_data_per_spend_anchor);
            self.revert_chain_state_with(sapling_shielded_data_shared_anchor);
            self.revert_chain_state_with(orchard_shielded_data);
        }
        let anchor = self
            .sprout_anchors_by_height
            .remove(&height)
            .expect("Sprout anchor must be present if block was added to chain");
        assert!(
            self.sprout_anchors.remove(&anchor),
            "Sprout anchor must be present if block was added to chain"
        );
        let anchor = self
            .sapling_anchors_by_height
            .remove(&height)
            .expect("Sapling anchor must be present if block was added to chain");
        assert!(
            self.sapling_anchors.remove(&anchor),
            "Sapling anchor must be present if block was added to chain"
        );
        let anchor = self
            .orchard_anchors_by_height
            .remove(&height)
            .expect("Orchard anchor must be present if block was added to chain");
        assert!(
            self.orchard_anchors.remove(&anchor),
            "Orchard anchor must be present if block was added to chain"
        );
    }
}

impl UpdateWith<HashMap<transparent::OutPoint, transparent::Utxo>> for Chain {
    fn update_chain_state_with(
        &mut self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<(), ValidateContextError> {
        self.created_utxos
            .extend(utxos.iter().map(|(k, v)| (*k, v.clone())));
        Ok(())
    }

    fn revert_chain_state_with(
        &mut self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) {
        self.created_utxos
            .retain(|outpoint, _| !utxos.contains_key(outpoint));
    }
}

impl UpdateWith<Vec<transparent::Input>> for Chain {
    fn update_chain_state_with(
        &mut self,
        inputs: &Vec<transparent::Input>,
    ) -> Result<(), ValidateContextError> {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    self.spent_utxos.insert(*outpoint);
                }
                transparent::Input::Coinbase { .. } => {}
            }
        }
        Ok(())
    }

    fn revert_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    assert!(
                        self.spent_utxos.remove(outpoint),
                        "spent_utxos must be present if block was added to chain"
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
    ) -> Result<(), ValidateContextError> {
        if let Some(joinsplit_data) = joinsplit_data {
            for cm in joinsplit_data.note_commitments() {
                self.sprout_note_commitment_tree.append(cm);
            }

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.sprout_nullifiers,
                joinsplit_data.nullifiers(),
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, joinsplit_data))]
    fn revert_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            // Note commitments are not removed from the tree here because we
            // don't support that operation yet. Instead, we recreate the tree
            // from the finalized tip in NonFinalizedState.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.sprout_nullifiers,
                joinsplit_data.nullifiers(),
            );
        }
    }
}

impl<AnchorV> UpdateWith<Option<sapling::ShieldedData<AnchorV>>> for Chain
where
    AnchorV: sapling::AnchorVariant + Clone,
{
    #[instrument(skip(self, sapling_shielded_data))]
    fn update_chain_state_with(
        &mut self,
        sapling_shielded_data: &Option<sapling::ShieldedData<AnchorV>>,
    ) -> Result<(), ValidateContextError> {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            for cm_u in sapling_shielded_data.note_commitments() {
                self.sapling_note_commitment_tree.append(*cm_u)?;
            }

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.sapling_nullifiers,
                sapling_shielded_data.nullifiers(),
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, sapling_shielded_data))]
    fn revert_chain_state_with(
        &mut self,
        sapling_shielded_data: &Option<sapling::ShieldedData<AnchorV>>,
    ) {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            // Note commitments are not removed from the tree here because we
            // don't support that operation yet. Instead, we recreate the tree
            // from the finalized tip in NonFinalizedState.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.sapling_nullifiers,
                sapling_shielded_data.nullifiers(),
            );
        }
    }
}

impl UpdateWith<Option<orchard::ShieldedData>> for Chain {
    #[instrument(skip(self, orchard_shielded_data))]
    fn update_chain_state_with(
        &mut self,
        orchard_shielded_data: &Option<orchard::ShieldedData>,
    ) -> Result<(), ValidateContextError> {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            for cm_x in orchard_shielded_data.note_commitments() {
                self.orchard_note_commitment_tree.append(*cm_x)?;
            }

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.orchard_nullifiers,
                orchard_shielded_data.nullifiers(),
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, orchard_shielded_data))]
    fn revert_chain_state_with(&mut self, orchard_shielded_data: &Option<orchard::ShieldedData>) {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            // Note commitments are not removed from the tree here because we
            // don't support that operation yet. Instead, we recreate the tree
            // from the finalized tip in NonFinalizedState.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.orchard_nullifiers,
                orchard_shielded_data.nullifiers(),
            );
        }
    }
}

impl Ord for Chain {
    /// Chain order for the [`NonFinalizedState`]'s `chain_set`.
    /// Chains with higher cumulative Proof of Work are [`Ordering::Greater`],
    /// breaking ties using the tip block hash.
    ///
    /// Despite the consensus rules, Zebra uses the tip block hash as a tie-breaker.
    /// Zebra blocks are downloaded in parallel, so download timestamps may not be unique.
    /// (And Zebra currently doesn't track download times, because [`Block`]s are immutable.)
    ///
    /// This departure from the consensus rules may delay network convergence,
    /// for as long as the greater hash belongs to the later mined block.
    /// But Zebra nodes should converge as soon as the tied work is broken.
    ///
    /// "At a given point in time, each full validator is aware of a set of candidate blocks.
    /// These form a tree rooted at the genesis block, where each node in the tree
    /// refers to its parent via the hashPrevBlock block header field.
    ///
    /// A path from the root toward the leaves of the tree consisting of a sequence
    /// of one or more valid blocks consistent with consensus rules,
    /// is called a valid block chain.
    ///
    /// In order to choose the best valid block chain in its view of the overall block tree,
    /// a node sums the work ... of all blocks in each valid block chain,
    /// and considers the valid block chain with greatest total work to be best.
    ///
    /// To break ties between leaf blocks, a node will prefer the block that it received first.
    ///
    /// The consensus protocol is designed to ensure that for any given block height,
    /// the vast majority of nodes should eventually agree on their best valid block chain
    /// up to that height."
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#blockchain
    ///
    /// # Panics
    ///
    /// If two chains compare equal.
    ///
    /// This panic enforces the `NonFinalizedState.chain_set` unique chain invariant.
    ///
    /// If the chain set contains duplicate chains, the non-finalized state might
    /// handle new blocks or block finalization incorrectly.
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

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Chain {
    /// Chain equality for the [`NonFinalizedState`]'s `chain_set`,
    /// using proof of work, then the tip block hash as a tie-breaker.
    ///
    /// # Panics
    ///
    /// If two chains compare equal.
    ///
    /// See [`Chain::cmp`] for details.
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl Eq for Chain {}
