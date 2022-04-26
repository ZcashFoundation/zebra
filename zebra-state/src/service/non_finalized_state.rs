//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

use std::{collections::BTreeSet, mem, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    history_tree::HistoryTree,
    orchard,
    parameters::Network,
    sapling, sprout, transparent,
};

use crate::{
    request::ContextuallyValidBlock,
    service::{check, finalized_state::ZebraDb},
    FinalizedBlock, PreparedBlock, ValidateContextError,
};

mod chain;
mod queued_blocks;

#[cfg(test)]
mod tests;

pub use queued_blocks::QueuedBlocks;

pub(crate) use chain::Chain;

/// The state of the chains in memory, including queued blocks.
#[derive(Debug, Clone)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains, in ascending order.
    ///
    /// The best chain is `chain_set.last()` or `chain_set.iter().next_back()`.
    pub chain_set: BTreeSet<Arc<Chain>>,

    /// The configured Zcash network.
    //
    // Note: this field is currently unused, but it's useful for debugging.
    pub network: Network,
}

impl NonFinalizedState {
    /// Returns a new non-finalized state for `network`.
    pub fn new(network: Network) -> NonFinalizedState {
        NonFinalizedState {
            chain_set: Default::default(),
            network,
        }
    }

    /// Is the internal state of `self` the same as `other`?
    ///
    /// [`Chain`] has a custom [`Eq`] implementation based on proof of work,
    /// which is used to select the best chain. So we can't derive [`Eq`] for [`NonFinalizedState`].
    ///
    /// Unlike the custom trait impl, this method returns `true` if the entire internal state
    /// of two non-finalized states is equal.
    ///
    /// If the internal states are different, it returns `false`,
    /// even if the chains and blocks are equal.
    #[cfg(test)]
    pub(crate) fn eq_internal_state(&self, other: &NonFinalizedState) -> bool {
        // this method must be updated every time a field is added to NonFinalizedState

        self.chain_set.len() == other.chain_set.len()
            && self
                .chain_set
                .iter()
                .zip(other.chain_set.iter())
                .all(|(self_chain, other_chain)| self_chain.eq_internal_state(other_chain))
            && self.network == other.network
    }

    /// Finalize the lowest height block in the non-finalized portion of the best
    /// chain and update all side-chains to match.
    pub fn finalize(&mut self) -> FinalizedBlock {
        // Chain::cmp uses the partial cumulative work, and the hash of the tip block.
        // Neither of these fields has interior mutability.
        // (And when the tip block is dropped for a chain, the chain is also dropped.)
        #[allow(clippy::mutable_key_type)]
        let chains = mem::take(&mut self.chain_set);
        let mut chains = chains.into_iter();

        // extract best chain
        let mut best_chain = chains.next_back().expect("there's at least one chain");
        // clone if required
        let write_best_chain = Arc::make_mut(&mut best_chain);

        // extract the rest into side_chains so they can be mutated
        let side_chains = chains;

        // remove the lowest height block from the best_chain to be finalized
        let finalizing = write_best_chain.pop_root();

        // add best_chain back to `self.chain_set`
        if !best_chain.is_empty() {
            self.chain_set.insert(best_chain);
        }

        // for each remaining chain in side_chains
        for mut chain in side_chains {
            if chain.non_finalized_root_hash() != finalizing.hash {
                // If we popped the root, the chain would be empty or orphaned,
                // so just drop it now.
                drop(chain);

                continue;
            }

            // otherwise, the popped root block is the same as the finalizing block

            // clone if required
            let write_chain = Arc::make_mut(&mut chain);

            // remove the first block from `chain`
            let chain_start = write_chain.pop_root();
            assert_eq!(chain_start.hash, finalizing.hash);

            // add the chain back to `self.chain_set`
            self.chain_set.insert(chain);
        }

        self.update_metrics_for_chains();

        finalizing.into()
    }

    /// Commit block to the non-finalized state, on top of:
    /// - an existing chain's tip, or
    /// - a newly forked chain.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, prepared))]
    pub fn commit_block(
        &mut self,
        prepared: PreparedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        let parent_hash = prepared.block.header.previous_block_hash;
        let (height, hash) = (prepared.height, prepared.hash);

        let parent_chain = self.parent_chain(
            parent_hash,
            finalized_state.sprout_note_commitment_tree(),
            finalized_state.sapling_note_commitment_tree(),
            finalized_state.orchard_note_commitment_tree(),
            finalized_state.history_tree(),
        )?;

        // If the block is invalid, return the error,
        // and drop the cloned parent Arc, or newly created chain fork.
        let modified_chain = self.validate_and_commit(parent_chain, prepared, finalized_state)?;

        // If the block is valid:
        // - add the new chain fork or updated chain to the set of recent chains
        // - remove the parent chain, if it was in the chain set
        //   (if it was a newly created fork, it won't be in the chain set)
        self.chain_set.insert(modified_chain);
        self.chain_set
            .retain(|chain| chain.non_finalized_tip_hash() != parent_hash);

        self.update_metrics_for_committed_block(height, hash);

        Ok(())
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, prepared))]
    pub fn commit_new_chain(
        &mut self,
        prepared: PreparedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        let chain = Chain::new(
            self.network,
            finalized_state.sprout_note_commitment_tree(),
            finalized_state.sapling_note_commitment_tree(),
            finalized_state.orchard_note_commitment_tree(),
            finalized_state.history_tree(),
            finalized_state.finalized_value_pool(),
        );
        let (height, hash) = (prepared.height, prepared.hash);

        // If the block is invalid, return the error, and drop the newly created chain fork
        let chain = self.validate_and_commit(Arc::new(chain), prepared, finalized_state)?;

        // If the block is valid, add the new chain fork to the set of recent chains.
        self.chain_set.insert(chain);
        self.update_metrics_for_committed_block(height, hash);

        Ok(())
    }

    /// Contextually validate `prepared` using `finalized_state`.
    /// If validation succeeds, push `prepared` onto `new_chain`.
    ///
    /// `new_chain` should start as a clone of the parent chain fork,
    /// or the finalized tip.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, new_chain))]
    fn validate_and_commit(
        &self,
        new_chain: Arc<Chain>,
        prepared: PreparedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        let spent_utxos = check::utxo::transparent_spend(
            &prepared,
            &new_chain.unspent_utxos(),
            &new_chain.spent_utxos,
            finalized_state,
        )?;

        check::prepared_block_commitment_is_valid_for_chain_history(
            &prepared,
            self.network,
            &new_chain.history_tree,
        )?;

        check::anchors::anchors_refer_to_earlier_treestates(
            finalized_state,
            &new_chain,
            &prepared,
        )?;

        let contextual = ContextuallyValidBlock::with_block_and_spent_utxos(
            prepared.clone(),
            spent_utxos.clone(),
        )
        .map_err(|value_balance_error| {
            ValidateContextError::CalculateBlockChainValueChange {
                value_balance_error,
                height: prepared.height,
                block_hash: prepared.hash,
                transaction_count: prepared.block.transactions.len(),
                spent_utxo_count: spent_utxos.len(),
            }
        })?;

        // We're pretty sure the new block is valid,
        // so clone the inner chain if needed, then add the new block.
        Arc::try_unwrap(new_chain)
            .unwrap_or_else(|shared_chain| (*shared_chain).clone())
            .push(contextual)
            .map(Arc::new)
    }

    /// Returns the length of the non-finalized portion of the current best chain.
    pub fn best_chain_len(&self) -> u32 {
        self.best_chain()
            .expect("only called after inserting a block")
            .blocks
            .len() as u32
    }

    /// Returns `true` if `hash` is contained in the non-finalized portion of any
    /// known chain.
    pub fn any_chain_contains(&self, hash: &block::Hash) -> bool {
        self.chain_set
            .iter()
            .rev()
            .any(|chain| chain.height_by_hash.contains_key(hash))
    }

    /// Removes and returns the first chain satisfying the given predicate.
    ///
    /// If multiple chains satisfy the predicate, returns the chain with the highest difficulty.
    /// (Using the tip block hash tie-breaker.)
    fn find_chain<P>(&mut self, mut predicate: P) -> Option<&Arc<Chain>>
    where
        P: FnMut(&Chain) -> bool,
    {
        // Reverse the iteration order, to find highest difficulty chains first.
        self.chain_set.iter().rev().find(|chain| predicate(chain))
    }

    /// Returns the [`transparent::Utxo`] pointed to by the given
    /// [`transparent::OutPoint`] if it is present in any chain.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        for chain in self.chain_set.iter().rev() {
            if let Some(utxo) = chain.created_utxos.get(outpoint) {
                return Some(utxo.utxo.clone());
            }
        }

        None
    }

    /// Returns the `block` with the given hash in any chain.
    pub fn any_block_by_hash(&self, hash: block::Hash) -> Option<Arc<Block>> {
        for chain in self.chain_set.iter().rev() {
            if let Some(prepared) = chain
                .height_by_hash
                .get(&hash)
                .and_then(|height| chain.blocks.get(height))
            {
                return Some(prepared.block.clone());
            }
        }

        None
    }

    /// Returns the hash for a given `block::Height` if it is present in the best chain.
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.best_chain()?
            .blocks
            .get(&height)
            .map(|prepared| prepared.hash)
    }

    /// Returns the tip of the best chain.
    pub fn best_tip(&self) -> Option<(block::Height, block::Hash)> {
        let best_chain = self.best_chain()?;
        let height = best_chain.non_finalized_tip_height();
        let hash = best_chain.non_finalized_tip_hash();

        Some((height, hash))
    }

    /// Returns the block at the tip of the best chain.
    #[allow(dead_code)]
    pub fn best_tip_block(&self) -> Option<&ContextuallyValidBlock> {
        let best_chain = self.best_chain()?;

        best_chain.tip_block()
    }

    /// Returns the height of `hash` in the best chain.
    pub fn best_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        let best_chain = self.best_chain()?;
        let height = *best_chain.height_by_hash.get(&hash)?;
        Some(height)
    }

    /// Returns the height of `hash` in any chain.
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        for chain in self.chain_set.iter().rev() {
            if let Some(height) = chain.height_by_hash.get(&hash) {
                return Some(*height);
            }
        }

        None
    }

    /// Returns `true` if the best chain contains `sprout_nullifier`.
    #[cfg(test)]
    pub fn best_contains_sprout_nullifier(&self, sprout_nullifier: &sprout::Nullifier) -> bool {
        self.best_chain()
            .map(|best_chain| best_chain.sprout_nullifiers.contains(sprout_nullifier))
            .unwrap_or(false)
    }

    /// Returns `true` if the best chain contains `sapling_nullifier`.
    #[cfg(test)]
    pub fn best_contains_sapling_nullifier(&self, sapling_nullifier: &sapling::Nullifier) -> bool {
        self.best_chain()
            .map(|best_chain| best_chain.sapling_nullifiers.contains(sapling_nullifier))
            .unwrap_or(false)
    }

    /// Returns `true` if the best chain contains `orchard_nullifier`.
    #[cfg(test)]
    pub fn best_contains_orchard_nullifier(&self, orchard_nullifier: &orchard::Nullifier) -> bool {
        self.best_chain()
            .map(|best_chain| best_chain.orchard_nullifiers.contains(orchard_nullifier))
            .unwrap_or(false)
    }

    /// Return the non-finalized portion of the current best chain.
    pub(crate) fn best_chain(&self) -> Option<&Arc<Chain>> {
        self.chain_set.iter().next_back()
    }

    /// Return the chain whose tip block hash is `parent_hash`.
    ///
    /// The chain can be an existing chain in the non-finalized state or a freshly
    /// created fork, if needed.
    ///
    /// The trees must be the trees of the finalized tip.
    /// They are used to recreate the trees if a fork is needed.
    fn parent_chain(
        &mut self,
        parent_hash: block::Hash,
        sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
        history_tree: HistoryTree,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        match self.find_chain(|chain| chain.non_finalized_tip_hash() == parent_hash) {
            // Clone the existing Arc<Chain> in the non-finalized state
            Some(chain) => Ok(chain.clone()),
            // Create a new fork
            None => {
                // Check the lowest difficulty chains first,
                // because the fork could be closer to their tip.
                let fork_chain = self
                    .chain_set
                    .iter()
                    .find_map(|chain| {
                        chain
                            .fork(
                                parent_hash,
                                sprout_note_commitment_tree.clone(),
                                sapling_note_commitment_tree.clone(),
                                orchard_note_commitment_tree.clone(),
                                history_tree.clone(),
                            )
                            .transpose()
                    })
                    .expect(
                        "commit_block is only called with blocks that are ready to be committed",
                    )?;

                Ok(Arc::new(fork_chain))
            }
        }
    }

    /// Update the metrics after `block` is committed
    fn update_metrics_for_committed_block(&self, height: block::Height, hash: block::Hash) {
        metrics::counter!("state.memory.committed.block.count", 1);
        metrics::gauge!("state.memory.committed.block.height", height.0 as _);

        if self
            .best_chain()
            .unwrap()
            .blocks
            .iter()
            .next_back()
            .unwrap()
            .1
            .hash
            == hash
        {
            metrics::counter!("state.memory.best.committed.block.count", 1);
            metrics::gauge!("state.memory.best.committed.block.height", height.0 as _);
        }

        self.update_metrics_for_chains();
    }

    /// Update the metrics after `self.chain_set` is modified
    fn update_metrics_for_chains(&self) {
        metrics::gauge!("state.memory.chain.count", self.chain_set.len() as _);
        metrics::gauge!("state.memory.best.chain.length", self.best_chain_len() as _);
    }
}
