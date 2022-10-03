//! Queued blocks that are awaiting their parent block for verification.

use std::{
    collections::{hash_map::Drain, BTreeMap, HashMap, HashSet, VecDeque},
    mem,
};

use tokio::sync::oneshot;
use tracing::instrument;

use zebra_chain::{block, transparent};

use crate::{BoxError, FinalizedBlock, PreparedBlock};

#[cfg(test)]
mod tests;

/// A queued finalized block, and its corresponding [`Result`] channel.
pub type QueuedFinalized = (
    FinalizedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);

/// A queued non-finalized block, and its corresponding [`Result`] channel.
pub type QueuedNonFinalized = (
    PreparedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Debug, Default)]
pub struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedNonFinalized>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, HashSet<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, HashSet<block::Hash>>,
    /// Known UTXOs.
    known_utxos: HashMap<transparent::OutPoint, transparent::Utxo>,
}

impl QueuedBlocks {
    /// Queue a block for eventual verification and commit.
    ///
    /// # Panics
    ///
    /// - if a block with the same `block::Hash` has already been queued.
    #[instrument(skip(self), fields(height = ?new.0.height, hash = %new.0.hash))]
    pub fn queue(&mut self, new: QueuedNonFinalized) {
        let new_hash = new.0.hash;
        let new_height = new.0.height;
        let parent_hash = new.0.block.header.previous_block_hash;

        // Track known UTXOs in queued blocks.
        for (outpoint, ordered_utxo) in new.0.new_outputs.iter() {
            self.known_utxos
                .insert(*outpoint, ordered_utxo.utxo.clone());
        }

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

        tracing::trace!(%parent_hash, queued = %self.blocks.len(), "queued block");
        self.update_metrics();
    }

    /// Returns `true` if there are any queued children of `parent_hash`.
    #[instrument(skip(self), fields(%parent_hash))]
    pub fn has_queued_children(&self, parent_hash: block::Hash) -> bool {
        self.by_parent.contains_key(&parent_hash)
    }

    /// Dequeue and return all blocks that were waiting for the arrival of
    /// `parent`.
    #[instrument(skip(self), fields(%parent_hash))]
    pub fn dequeue_children(&mut self, parent_hash: block::Hash) -> Vec<QueuedNonFinalized> {
        let queued_children = self
            .by_parent
            .remove(&parent_hash)
            .unwrap_or_default()
            .into_iter()
            .map(|hash| {
                self.blocks
                    .remove(&hash)
                    .expect("block is present if its hash is in by_parent")
            })
            .collect::<Vec<_>>();

        for queued in &queued_children {
            self.by_height.remove(&queued.0.height);
            // TODO: only remove UTXOs if there are no queued blocks with that UTXO
            //       (known_utxos is best-effort, so this is ok for now)
            for outpoint in queued.0.new_outputs.keys() {
                self.known_utxos.remove(outpoint);
            }
        }

        tracing::trace!(
            dequeued = queued_children.len(),
            remaining = self.blocks.len(),
            "dequeued blocks"
        );
        self.update_metrics();

        queued_children
    }

    /// Remove all queued blocks whose height is less than or equal to the given
    /// `finalized_tip_height`.
    #[instrument(skip(self))]
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
            let (expired_block, expired_sender) =
                self.blocks.remove(&hash).expect("block is present");
            let parent_hash = &expired_block.block.header.previous_block_hash;

            // we don't care if the receiver was dropped
            let _ = expired_sender.send(Err(
                "pruned block at or below the finalized tip height".into()
            ));

            // TODO: only remove UTXOs if there are no queued blocks with that UTXO
            //       (known_utxos is best-effort, so this is ok for now)
            for outpoint in expired_block.new_outputs.keys() {
                self.known_utxos.remove(outpoint);
            }

            let parent_list = self
                .by_parent
                .get_mut(parent_hash)
                .expect("parent is present");

            if parent_list.len() == 1 {
                let removed = self
                    .by_parent
                    .remove(parent_hash)
                    .expect("parent is present");
                assert!(
                    removed.contains(&hash),
                    "hash must be present in parent hash list"
                );
            } else {
                assert!(
                    parent_list.remove(&hash),
                    "hash must be present in parent hash list"
                );
            }
        }

        tracing::trace!(num_blocks = %self.blocks.len(), "Finished pruning blocks at or beneath the finalized tip height");
        self.update_metrics();
    }

    /// Return the queued block if it has already been registered
    pub fn get_mut(&mut self, hash: &block::Hash) -> Option<&mut QueuedNonFinalized> {
        self.blocks.get_mut(hash)
    }

    /// Update metrics after the queue is modified
    fn update_metrics(&self) {
        if let Some(max_height) = self.by_height.keys().next_back() {
            metrics::gauge!("state.memory.queued.max.height", max_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("state.memory.queued.max.height", f64::NAN);
        }
        metrics::gauge!("state.memory.queued.block.count", self.blocks.len() as f64);
    }

    /// Try to look up this UTXO in any queued block.
    #[instrument(skip(self))]
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.known_utxos.get(outpoint).cloned()
    }

    /// Clears known_utxos, by_parent, and by_height, then drains blocks.
    /// Returns all key-value pairs of blocks as an iterator
    pub fn drain(&mut self) -> Drain<'_, block::Hash, QueuedNonFinalized> {
        self.known_utxos.clear();
        self.known_utxos.shrink_to_fit();
        self.by_parent.clear();
        self.by_parent.shrink_to_fit();
        self.by_height.clear();

        self.blocks.drain()
    }
}

#[derive(Debug, Default)]
pub(crate) struct SentHashes {
    /// A Vec of growable ring bufs to be used for pruning outdated hashes that are
    /// inserted in the order of their height in a given batch.
    sent_bufs: Vec<VecDeque<(block::Hash, block::Height)>>,

    /// The growable ring buf to be used for pruning outdated hashes that are mostly
    /// inserted in the order of their height for this batch of sent hashes
    curr_buf: VecDeque<(block::Hash, block::Height)>,

    /// Stores a set of hashes that have been sent to the block write task but
    /// may not be in the finalized state yet.
    set: HashSet<block::Hash>,
}

impl SentHashes {
    /// Inserts the hash into the set of sent hashes, and pushes the (hash, height)
    /// onto the current buf to be used for pruning outdated hashes.
    ///
    /// Assumes that hashes are added in the order of their height between `finish_batch` calls
    /// for efficient pruning.
    //
    // Trades off some memory usage to avoid iterating over the entire set when non-finalized blocks
    // are sent to the block write task, as would be the case with a HashMap.
    pub fn add_with_height(&mut self, hash: block::Hash, height: block::Height) {
        self.curr_buf.push_back((hash, height));
        self.set.insert(hash);
    }

    /// Replaces `curr_buf` with an empty VecDeque and pushes the previous `curr_buf` to `sent_bufs`
    pub fn finish_batch(&mut self) {
        if !self.curr_buf.is_empty() {
            self.sent_bufs.push(std::mem::take(&mut self.curr_buf));
        }
    }

    /// Iterates over each buf in `sent_bufs`, removing hashes from SentHashes until reaching
    /// the first hash with a height above the `height_bound`
    ///
    /// Assumes that hashes in each `sent_buf` will be added in order of their heights
    /// in order to remove all hashes that are below `height_bound`.
    pub fn prune_by_height(&mut self, height_bound: block::Height) {
        self.sent_bufs.retain_mut(|buf| {
            while let Some((hash, height)) = buf.pop_front() {
                if height > height_bound {
                    buf.push_front((hash, height));
                    return true;
                } else {
                    self.set.remove(&hash);
                }
            }

            false
        });

        self.set.shrink_to_fit();
    }

    /// Returns true if the set contains the `hash`
    pub fn contains(&self, hash: &block::Hash) -> bool {
        self.set.contains(hash)
    }
}
