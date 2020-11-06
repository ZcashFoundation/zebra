use std::{
    collections::{BTreeMap, HashMap, HashSet},
    mem,
};

use tracing::instrument;
use zebra_chain::block;

use crate::service::QueuedBlock;

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Default)]
pub struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedBlock>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, HashSet<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, HashSet<block::Hash>>,
}

impl QueuedBlocks {
    /// Queue a block for eventual verification and commit.
    ///
    /// # Panics
    ///
    /// - if a block with the same `block::Hash` has already been queued.
    pub fn queue(&mut self, new: QueuedBlock) {
        let new_hash = new.block.hash();
        let new_height = new
            .block
            .coinbase_height()
            .expect("validated non-finalized blocks have a coinbase height");
        let parent_hash = new.block.header.previous_block_hash;

        let replaced = self.blocks.insert(new_hash, new);
        assert!(replaced.is_none(), "hashes must be unique");
        let inserted = self
            .by_height
            .entry(new_height)
            .or_default()
            .insert(new_hash);
        assert!(inserted, "hashes must be unique");
        let inserted = self
            .by_parent
            .entry(parent_hash)
            .or_default()
            .insert(new_hash);
        assert!(inserted, "hashes must be unique");

        tracing::trace!(num_blocks = %self.blocks.len(), %parent_hash, ?new_height,  "Finished queueing a new block");
    }

    /// Dequeue and return all blocks that were waiting for the arrival of
    /// `parent`.
    #[instrument(skip(self))]
    pub fn dequeue_children(&mut self, parent: block::Hash) -> Vec<QueuedBlock> {
        let queued_children = self
            .by_parent
            .remove(&parent)
            .unwrap_or_default()
            .into_iter()
            .map(|hash| {
                self.blocks
                    .remove(&hash)
                    .expect("block is present if its hash is in by_parent")
            })
            .collect::<Vec<_>>();

        for queued in &queued_children {
            let height = queued.block.coinbase_height().unwrap();
            self.by_height.remove(&height);
        }

        tracing::trace!(num_blocks = %self.blocks.len(), "Finished dequeuing blocks waiting for parent hash",);

        queued_children
    }

    /// Remove all queued blocks whose height is less than or equal to the given
    /// `finalized_tip_height`.
    pub fn prune_by_height(&mut self, finalized_tip_height: block::Height) {
        // split_off returns the values _greater than or equal to_ the key. What
        // we need is the keys that are less than or equal to
        // `finalized_tip_height`. To get this we have split at
        // `finalized_tip_height + 1` and swap the removed portion of the list
        // with the remainder.
        let split_height = finalized_tip_height + 1;
        let split_height =
            split_height.expect("height after finalized tip won't exceed max height");
        let mut by_height = self.by_height.split_off(&split_height);
        mem::swap(&mut self.by_height, &mut by_height);

        for hash in by_height.into_iter().flat_map(|(_, hashes)| hashes) {
            let expired = self.blocks.remove(&hash).expect("block is present");
            let parent_hash = &expired.block.header.previous_block_hash;
            self.by_parent
                .get_mut(parent_hash)
                .expect("parent is present")
                .remove(&hash);
        }
    }

    /// Return the queued block if it has already been registered
    pub fn get_mut(&mut self, hash: &block::Hash) -> Option<&mut QueuedBlock> {
        self.blocks.get_mut(&hash)
    }
}
