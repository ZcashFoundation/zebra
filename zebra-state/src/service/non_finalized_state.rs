//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

use std::{
    collections::{BTreeSet, HashMap},
    mem,
    sync::Arc,
};

use zebra_chain::{
    block::{self, Block},
    history_tree::HistoryTree,
    orchard,
    parameters::Network,
    sapling, sprout, transparent,
};

use crate::{
    request::{ContextuallyValidBlock, FinalizedWithTrees},
    service::{check, finalized_state::ZebraDb},
    PreparedBlock, ValidateContextError,
};

mod chain;

#[cfg(test)]
mod tests;

pub(crate) use chain::Chain;

/// The state of the chains in memory, including queued blocks.
///
/// Clones of the non-finalized state contain independent copies of the chains.
/// This is different from `FinalizedState::clone()`,
/// which returns a shared reference to the database.
///
/// Most chain data is clone-on-write using [`Arc`].
#[derive(Clone, Debug)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains, in ascending order.
    ///
    /// The best chain is `chain_set.last()` or `chain_set.iter().next_back()`.
    pub chain_set: BTreeSet<Arc<Chain>>,

    /// The configured Zcash network.
    pub network: Network,

    #[cfg(feature = "getblocktemplate-rpcs")]
    /// Configures the non-finalized state to count metrics.
    ///
    /// Used for skipping metrics counting when testing block proposals
    /// with a commit to a cloned non-finalized state.
    pub should_count_metrics: bool,
}

impl NonFinalizedState {
    /// Returns a new non-finalized state for `network`.
    pub fn new(network: Network) -> NonFinalizedState {
        NonFinalizedState {
            chain_set: Default::default(),
            network,
            #[cfg(feature = "getblocktemplate-rpcs")]
            should_count_metrics: true,
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
    pub fn finalize(&mut self) -> FinalizedWithTrees {
        // Chain::cmp uses the partial cumulative work, and the hash of the tip block.
        // Neither of these fields has interior mutability.
        // (And when the tip block is dropped for a chain, the chain is also dropped.)
        #[allow(clippy::mutable_key_type)]
        let chains = mem::take(&mut self.chain_set);
        let mut chains = chains.into_iter();

        // extract best chain
        let mut best_chain = chains.next_back().expect("there's at least one chain");

        // clone if required
        let mut_best_chain = Arc::make_mut(&mut best_chain);

        // extract the rest into side_chains so they can be mutated
        let side_chains = chains;

        // Pop the lowest height block from the best chain to be finalized, and
        // also obtain its associated treestate.
        let (best_chain_root, root_treestate) = mut_best_chain.pop_root();

        // add best_chain back to `self.chain_set`
        if !best_chain.is_empty() {
            self.chain_set.insert(best_chain);
        }

        // for each remaining chain in side_chains
        for mut side_chain in side_chains {
            if side_chain.non_finalized_root_hash() != best_chain_root.hash {
                // If we popped the root, the chain would be empty or orphaned,
                // so just drop it now.
                drop(side_chain);

                continue;
            }

            // otherwise, the popped root block is the same as the finalizing block

            // clone if required
            let mut_side_chain = Arc::make_mut(&mut side_chain);

            // remove the first block from `chain`
            let (side_chain_root, _treestate) = mut_side_chain.pop_root();
            assert_eq!(side_chain_root.hash, best_chain_root.hash);

            // add the chain back to `self.chain_set`
            self.chain_set.insert(side_chain);
        }

        self.update_metrics_for_chains();

        // Add the treestate to the finalized block.
        FinalizedWithTrees::new(best_chain_root, root_treestate)
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
        // Reads from disk
        //
        // TODO: if these disk reads show up in profiles, run them in parallel, using std::thread::spawn()
        let spent_utxos = check::utxo::transparent_spend(
            &prepared,
            &new_chain.unspent_utxos(),
            &new_chain.spent_utxos,
            finalized_state,
        )?;

        // Reads from disk
        check::anchors::block_sapling_orchard_anchors_refer_to_final_treestates(
            finalized_state,
            &new_chain,
            &prepared,
        )?;

        // Reads from disk
        let sprout_final_treestates = check::anchors::block_fetch_sprout_final_treestates(
            finalized_state,
            &new_chain,
            &prepared,
        );

        // Quick check that doesn't read from disk
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

        Self::validate_and_update_parallel(new_chain, contextual, sprout_final_treestates)
    }

    /// Validate `contextual` and update `new_chain`, doing CPU-intensive work in parallel batches.
    #[allow(clippy::unwrap_in_result)]
    #[tracing::instrument(skip(new_chain, sprout_final_treestates))]
    fn validate_and_update_parallel(
        new_chain: Arc<Chain>,
        contextual: ContextuallyValidBlock,
        sprout_final_treestates: HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        let mut block_commitment_result = None;
        let mut sprout_anchor_result = None;
        let mut chain_push_result = None;

        // Clone function arguments for different threads
        let block = contextual.block.clone();
        let network = new_chain.network();
        let history_tree = new_chain.history_tree.clone();

        let block2 = contextual.block.clone();
        let height = contextual.height;
        let transaction_hashes = contextual.transaction_hashes.clone();

        rayon::in_place_scope_fifo(|scope| {
            scope.spawn_fifo(|_scope| {
                block_commitment_result = Some(check::block_commitment_is_valid_for_chain_history(
                    block,
                    network,
                    &history_tree,
                ));
            });

            scope.spawn_fifo(|_scope| {
                sprout_anchor_result =
                    Some(check::anchors::block_sprout_anchors_refer_to_treestates(
                        sprout_final_treestates,
                        block2,
                        transaction_hashes,
                        height,
                    ));
            });

            // We're pretty sure the new block is valid,
            // so clone the inner chain if needed, then add the new block.
            //
            // Pushing a block onto a Chain can launch additional parallel batches.
            // TODO: should we pass _scope into Chain::push()?
            scope.spawn_fifo(|_scope| {
                let new_chain = Arc::try_unwrap(new_chain)
                    .unwrap_or_else(|shared_chain| (*shared_chain).clone());
                chain_push_result = Some(new_chain.push(contextual).map(Arc::new));
            });
        });

        // Don't return the updated Chain unless all the parallel results were Ok
        block_commitment_result.expect("scope has finished")?;
        sprout_anchor_result.expect("scope has finished")?;

        chain_push_result.expect("scope has finished")
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
    #[allow(dead_code)]
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
    ///
    /// UTXOs are returned regardless of whether they have been spent.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.chain_set
            .iter()
            .rev()
            .find_map(|chain| chain.created_utxo(outpoint))
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
    #[allow(dead_code)]
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.best_chain()?
            .blocks
            .get(&height)
            .map(|prepared| prepared.hash)
    }

    /// Returns the tip of the best chain.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(clippy::unwrap_in_result)]
    fn parent_chain(
        &mut self,
        parent_hash: block::Hash,
        sprout_note_commitment_tree: Arc<sprout::tree::NoteCommitmentTree>,
        sapling_note_commitment_tree: Arc<sapling::tree::NoteCommitmentTree>,
        orchard_note_commitment_tree: Arc<orchard::tree::NoteCommitmentTree>,
        history_tree: Arc<HistoryTree>,
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
                    .transpose()?
                    .ok_or(ValidateContextError::NotReadyToBeCommitted)?;

                Ok(Arc::new(fork_chain))
            }
        }
    }

    /// Update the metrics after `block` is committed
    fn update_metrics_for_committed_block(&self, height: block::Height, hash: block::Hash) {
        #[cfg(feature = "getblocktemplate-rpcs")]
        if !self.should_count_metrics {
            return;
        }

        metrics::counter!("state.memory.committed.block.count", 1);
        metrics::gauge!("state.memory.committed.block.height", height.0 as f64);

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
            metrics::gauge!("state.memory.best.committed.block.height", height.0 as f64);
        }

        self.update_metrics_for_chains();
    }

    /// Update the metrics after `self.chain_set` is modified
    fn update_metrics_for_chains(&self) {
        #[cfg(feature = "getblocktemplate-rpcs")]
        if !self.should_count_metrics {
            return;
        }

        metrics::gauge!("state.memory.chain.count", self.chain_set.len() as f64);
        metrics::gauge!(
            "state.memory.best.chain.length",
            self.best_chain_len() as f64,
        );
    }
}
