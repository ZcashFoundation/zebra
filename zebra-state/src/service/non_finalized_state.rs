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
    parameters::Network,
    transaction::{self, Transaction},
    transparent,
};

use crate::{FinalizedBlock, HashOrHeight, PreparedBlock, Utxo, ValidateContextError};

use self::chain::Chain;

/// The state of the chains in memory, incuding queued blocks.
#[derive(Default)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains, in ascending order.
    ///
    /// The best chain is `chain_set.last()` or `chain_set.iter().next_back()`.
    pub chain_set: BTreeSet<Box<Chain>>,
    /// The configured Zcash network
    pub network: Network,
}

impl NonFinalizedState {
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

    /// Commit block to the non-finalized state.
    pub fn commit_block(&mut self, prepared: PreparedBlock) -> Result<(), ValidateContextError> {
        let parent_hash = prepared.block.header.previous_block_hash;
        let (height, hash) = (prepared.height, prepared.hash);

        let mandatory_checkpoint = self.network.mandatory_checkpoint_height();
        if height <= mandatory_checkpoint {
            panic!(
                "invalid non-finalized block height: the canopy checkpoint is mandatory, pre-canopy blocks, and the canopy activation block, must be committed to the state as finalized blocks"
            );
        }

        let mut parent_chain = self.parent_chain(parent_hash)?;

        parent_chain.push(prepared)?;
        self.chain_set.insert(parent_chain);
        self.update_metrics_for_committed_block(height, hash);
        Ok(())
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    pub fn commit_new_chain(
        &mut self,
        prepared: PreparedBlock,
    ) -> Result<(), ValidateContextError> {
        let mut chain = Chain::default();
        let (height, hash) = (prepared.height, prepared.hash);
        chain.push(prepared)?;
        self.chain_set.insert(Box::new(chain));
        self.update_metrics_for_committed_block(height, hash);
        Ok(())
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
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<Utxo> {
        for chain in self.chain_set.iter().rev() {
            if let Some(ordered_utxo) = chain.created_utxos.get(outpoint) {
                return Some(ordered_utxo.utxo.clone());
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
    fn parent_chain(
        &mut self,
        parent_hash: block::Hash,
    ) -> Result<Box<Chain>, ValidateContextError> {
        match self.take_chain_if(|chain| chain.non_finalized_tip_hash() == parent_hash) {
            // An existing chain in the non-finalized state
            Some(chain) => Ok(chain),
            // Create a new fork
            None => Ok(Box::new(
                self.chain_set
                    .iter()
                    .find_map(|chain| chain.fork(parent_hash).transpose())
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
