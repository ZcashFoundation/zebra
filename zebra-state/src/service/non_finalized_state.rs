//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

mod chain;
mod queued_blocks;

#[cfg(test)]
mod tests;

pub use queued_blocks::QueuedBlocks;

use std::{collections::BTreeSet, mem, ops::Deref, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    history_tree::HistoryTree,
    orchard,
    parameters::Network,
    sapling,
    transaction::{self, Transaction},
    transparent,
};

#[cfg(test)]
use zebra_chain::sprout;

use crate::{
    request::ContextuallyValidBlock, FinalizedBlock, HashOrHeight, PreparedBlock,
    ValidateContextError,
};

use self::chain::Chain;

use super::{check, finalized_state::FinalizedState};

/// The state of the chains in memory, incuding queued blocks.
#[derive(Debug, Clone)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains, in ascending order.
    ///
    /// The best chain is `chain_set.last()` or `chain_set.iter().next_back()`.
    pub chain_set: BTreeSet<Box<Chain>>,

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
        let chains = mem::take(&mut self.chain_set);
        let mut chains = chains.into_iter();

        // extract best chain
        let mut best_chain = chains.next_back().expect("there's at least one chain");
        // extract the rest into side_chains so they can be mutated
        let side_chains = chains;

        // remove the lowest height block from the best_chain to be finalized
        let finalizing = best_chain.pop_root();

        // add best_chain back to `self.chain_set`
        if !best_chain.is_empty() {
            self.chain_set.insert(best_chain);
        }

        // for each remaining chain in side_chains
        for mut chain in side_chains {
            // remove the first block from `chain`
            let chain_start = chain.pop_root();
            // if block equals finalized_block
            if !chain.is_empty() && chain_start.hash == finalizing.hash {
                // add the chain back to `self.chain_set`
                self.chain_set.insert(chain);
            } else {
                // else discard `chain`
                drop(chain);
            }
        }

        self.update_metrics_for_chains();

        finalizing.into()
    }

    /// Commit block to the non-finalized state, on top of:
    /// - an existing chain's tip, or
    /// - a newly forked chain.
    pub fn commit_block(
        &mut self,
        prepared: PreparedBlock,
        finalized_state: &FinalizedState,
    ) -> Result<(), ValidateContextError> {
        let parent_hash = prepared.block.header.previous_block_hash;
        let (height, hash) = (prepared.height, prepared.hash);

        let parent_chain = self.parent_chain(
            parent_hash,
            finalized_state.sapling_note_commitment_tree(),
            finalized_state.orchard_note_commitment_tree(),
            finalized_state.history_tree(),
        )?;

        // We might have taken a chain, so all validation must happen within
        // validate_and_commit, so that the chain is restored correctly.
        match self.validate_and_commit(*parent_chain.clone(), prepared, finalized_state) {
            Ok(child_chain) => {
                // if the block is valid, keep the child chain, and drop the parent chain
                self.chain_set.insert(Box::new(child_chain));
                self.update_metrics_for_committed_block(height, hash);
                Ok(())
            }
            Err(err) => {
                // if the block is invalid, restore the unmodified parent chain
                // (the child chain might have been modified before the error)
                //
                // If the chain was forked, this adds an extra chain to the
                // chain set. This extra chain will eventually get deleted
                // (or re-used for a valid fork).
                self.chain_set.insert(parent_chain);
                Err(err)
            }
        }
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    pub fn commit_new_chain(
        &mut self,
        prepared: PreparedBlock,
        finalized_state: &FinalizedState,
    ) -> Result<(), ValidateContextError> {
        let chain = Chain::new(
            self.network,
            finalized_state.sapling_note_commitment_tree(),
            finalized_state.orchard_note_commitment_tree(),
            finalized_state.history_tree(),
            finalized_state.current_value_pool(),
        );
        let (height, hash) = (prepared.height, prepared.hash);

        // if the block is invalid, drop the newly created chain fork
        let chain = self.validate_and_commit(chain, prepared, finalized_state)?;
        self.chain_set.insert(Box::new(chain));
        self.update_metrics_for_committed_block(height, hash);
        Ok(())
    }

    /// Contextually validate `prepared` using `finalized_state`.
    /// If validation succeeds, push `prepared` onto `parent_chain`.
    fn validate_and_commit(
        &self,
        parent_chain: Chain,
        prepared: PreparedBlock,
        finalized_state: &FinalizedState,
    ) -> Result<Chain, ValidateContextError> {
        let spent_utxos = check::utxo::transparent_spend(
            &prepared,
            &parent_chain.unspent_utxos(),
            &parent_chain.spent_utxos,
            finalized_state,
        )?;

        check::block_commitment_is_valid_for_chain_history(
            &prepared,
            self.network,
            &parent_chain.history_tree,
        )?;

        let contextual = ContextuallyValidBlock::with_block_and_spent_utxos(prepared, spent_utxos)
            .map_err(ValidateContextError::CalculateBlockChainValueChange)?;

        parent_chain.push(contextual)
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

    /// Remove and return the first chain satisfying the given predicate.
    fn take_chain_if<F>(&mut self, predicate: F) -> Option<Box<Chain>>
    where
        F: Fn(&Chain) -> bool,
    {
        let chains = mem::take(&mut self.chain_set);
        let mut best_chain_iter = chains.into_iter().rev();

        while let Some(next_best_chain) = best_chain_iter.next() {
            // if the predicate says we should remove it
            if predicate(&next_best_chain) {
                // add back the remaining chains
                for remaining_chain in best_chain_iter {
                    self.chain_set.insert(remaining_chain);
                }

                // and return the chain
                return Some(next_best_chain);
            } else {
                // add the chain back to the set and continue
                self.chain_set.insert(next_best_chain);
            }
        }

        None
    }

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present in any chain.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        for chain in self.chain_set.iter().rev() {
            if let Some(utxo) = chain.created_utxos.get(outpoint) {
                return Some(utxo.clone());
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

    /// Returns the `block` at a given height or hash in the best chain.
    pub fn best_block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        let best_chain = self.best_chain()?;
        let height =
            hash_or_height.height_or_else(|hash| best_chain.height_by_hash.get(&hash).cloned())?;

        best_chain
            .blocks
            .get(&height)
            .map(|prepared| prepared.block.clone())
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

    /// Returns the given transaction if it exists in the best chain.
    pub fn best_transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        let best_chain = self.best_chain()?;
        best_chain
            .tx_by_hash
            .get(&hash)
            .map(|(height, index)| best_chain.blocks[height].block.transactions[*index].clone())
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

    /// Return the non-finalized portion of the current best chain
    fn best_chain(&self) -> Option<&Chain> {
        self.chain_set
            .iter()
            .next_back()
            .map(|box_chain| box_chain.deref())
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
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
        history_tree: HistoryTree,
    ) -> Result<Box<Chain>, ValidateContextError> {
        match self.take_chain_if(|chain| chain.non_finalized_tip_hash() == parent_hash) {
            // An existing chain in the non-finalized state
            Some(chain) => Ok(chain),
            // Create a new fork
            None => Ok(Box::new(
                self.chain_set
                    .iter()
                    .find_map(|chain| {
                        chain
                            .fork(
                                parent_hash,
                                sapling_note_commitment_tree.clone(),
                                orchard_note_commitment_tree.clone(),
                                history_tree.clone(),
                            )
                            .transpose()
                    })
                    .expect(
                        "commit_block is only called with blocks that are ready to be commited",
                    )?,
            )),
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
