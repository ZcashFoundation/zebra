//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

mod chain;
mod queued_blocks;

pub use queued_blocks::QueuedBlocks;

use std::{collections::BTreeSet, mem, ops::Deref, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    transaction::{self, Transaction},
    transparent,
};

use crate::{FinalizedBlock, HashOrHeight, PreparedBlock, Utxo};

use self::chain::Chain;

/// The state of the chains in memory, incuding queued blocks.
#[derive(Default)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains, in ascending order.
    ///
    /// The best chain is `chain_set.last()` or `chain_set.iter().next_back()`.
    chain_set: BTreeSet<Box<Chain>>,
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
    pub fn commit_block(&mut self, prepared: PreparedBlock) {
        let parent_hash = prepared.block.header.previous_block_hash;

        let mut parent_chain = self
            .take_chain_if(|chain| chain.non_finalized_tip_hash() == parent_hash)
            .or_else(|| {
                self.chain_set
                    .iter()
                    .find_map(|chain| chain.fork(parent_hash))
                    .map(Box::new)
            })
            .expect("commit_block is only called with blocks that are ready to be commited");

        let (height, hash) = (prepared.height, prepared.hash);
        parent_chain.push(prepared);
        self.chain_set.insert(parent_chain);
        self.update_metrics_for_committed_block(height, hash);
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    pub fn commit_new_chain(&mut self, prepared: PreparedBlock) {
        let mut chain = Chain::default();
        let (height, hash) = (prepared.height, prepared.hash);
        chain.push(prepared);
        self.chain_set.insert(Box::new(chain));
        self.update_metrics_for_committed_block(height, hash);
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
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<Utxo> {
        for chain in self.chain_set.iter().rev() {
            if let Some(output) = chain.created_utxos.get(outpoint) {
                return Some(output.clone());
            }
        }

        None
    }

    /// Returns the `block` with the given hash in the any chain.
    pub fn block_by_hash(&self, hash: block::Hash) -> Option<Arc<Block>> {
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
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        let best_chain = self.best_chain()?;
        let height =
            hash_or_height.height_or_else(|hash| best_chain.height_by_hash.get(&hash).cloned())?;

        best_chain
            .blocks
            .get(&height)
            .map(|prepared| prepared.block.clone())
    }

    /// Returns the hash for a given `block::Height` if it is present in the best chain.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        self.block(height.into()).map(|block| block.hash())
    }

    /// Returns the tip of the best chain.
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
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
    pub fn transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
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

#[cfg(test)]
mod tests {
    use zebra_chain::serialization::ZcashDeserializeInto;
    use zebra_test::prelude::*;

    use crate::tests::{FakeChainHelper, Prepare};

    use self::assert_eq;
    use super::*;

    #[test]
    fn best_chain_wins() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
        let block2 = block1.make_fake_child().set_work(10);
        let child = block1.make_fake_child().set_work(1);

        let expected_hash = block2.hash();

        let mut state = NonFinalizedState::default();
        state.commit_new_chain(block2.prepare());
        state.commit_new_chain(child.prepare());

        let best_chain = state.best_chain().unwrap();
        assert!(best_chain.height_by_hash.contains_key(&expected_hash));

        Ok(())
    }

    #[test]
    fn finalize_pops_from_best_chain() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
        let block2 = block1.make_fake_child().set_work(10);
        let child = block1.make_fake_child().set_work(1);

        let mut state = NonFinalizedState::default();
        state.commit_new_chain(block1.clone().prepare());
        state.commit_block(block2.clone().prepare());
        state.commit_block(child.prepare());

        let finalized = state.finalize();
        assert_eq!(block1, finalized.block);

        let finalized = state.finalize();
        assert_eq!(block2, finalized.block);

        assert!(state.best_chain().is_none());

        Ok(())
    }

    #[test]
    // This test gives full coverage for `take_chain_if`
    fn commit_block_extending_best_chain_doesnt_drop_worst_chains() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;
        let block2 = block1.make_fake_child().set_work(10);
        let child1 = block1.make_fake_child().set_work(1);
        let child2 = block2.make_fake_child().set_work(1);

        let mut state = NonFinalizedState::default();
        assert_eq!(0, state.chain_set.len());
        state.commit_new_chain(block1.prepare());
        assert_eq!(1, state.chain_set.len());
        state.commit_block(block2.prepare());
        assert_eq!(1, state.chain_set.len());
        state.commit_block(child1.prepare());
        assert_eq!(2, state.chain_set.len());
        state.commit_block(child2.prepare());
        assert_eq!(2, state.chain_set.len());

        Ok(())
    }

    #[test]
    fn shorter_chain_can_be_best_chain() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;

        let long_chain_block1 = block1.make_fake_child().set_work(1);
        let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);

        let short_chain_block = block1.make_fake_child().set_work(3);

        let mut state = NonFinalizedState::default();
        state.commit_new_chain(block1.prepare());
        state.commit_block(long_chain_block1.prepare());
        state.commit_block(long_chain_block2.prepare());
        state.commit_block(short_chain_block.prepare());
        assert_eq!(2, state.chain_set.len());

        assert_eq!(2, state.best_chain_len());

        Ok(())
    }

    #[test]
    fn longer_chain_with_more_work_wins() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;

        let long_chain_block1 = block1.make_fake_child().set_work(1);
        let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);
        let long_chain_block3 = long_chain_block2.make_fake_child().set_work(1);
        let long_chain_block4 = long_chain_block3.make_fake_child().set_work(1);

        let short_chain_block = block1.make_fake_child().set_work(3);

        let mut state = NonFinalizedState::default();
        state.commit_new_chain(block1.prepare());
        state.commit_block(long_chain_block1.prepare());
        state.commit_block(long_chain_block2.prepare());
        state.commit_block(long_chain_block3.prepare());
        state.commit_block(long_chain_block4.prepare());
        state.commit_block(short_chain_block.prepare());
        assert_eq!(2, state.chain_set.len());

        assert_eq!(5, state.best_chain_len());

        Ok(())
    }

    #[test]
    fn equal_length_goes_to_more_work() -> Result<()> {
        zebra_test::init();

        let block1: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into()?;

        let less_work_child = block1.make_fake_child().set_work(1);
        let more_work_child = block1.make_fake_child().set_work(3);
        let expected_hash = more_work_child.hash();

        let mut state = NonFinalizedState::default();
        state.commit_new_chain(block1.prepare());
        state.commit_block(less_work_child.prepare());
        state.commit_block(more_work_child.prepare());
        assert_eq!(2, state.chain_set.len());

        let tip_hash = state.tip().unwrap().1;
        assert_eq!(expected_hash, tip_hash);

        Ok(())
    }
}
