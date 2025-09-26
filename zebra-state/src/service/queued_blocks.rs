//! Queued blocks that are awaiting their parent block for verification.

use std::{
    collections::{hash_map::Drain, BTreeMap, HashMap, HashSet, VecDeque},
    iter, mem,
};

use tokio::sync::oneshot;
use tracing::instrument;

use zebra_chain::{block, transparent};

use crate::{
    error::QueueAndCommitError, BoxError, CheckpointVerifiedBlock, CommitSemanticallyVerifiedError,
    NonFinalizedState, SemanticallyVerifiedBlock,
};

#[cfg(test)]
mod tests;

/// A queued checkpoint verified block, and its corresponding [`Result`] channel.
pub type QueuedCheckpointVerified = (
    CheckpointVerifiedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);

/// A queued semantically verified block, and its corresponding [`Result`] channel.
pub type QueuedSemanticallyVerified = (
    SemanticallyVerifiedBlock,
    oneshot::Sender<Result<block::Hash, CommitSemanticallyVerifiedError>>,
);

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Debug, Default)]
pub struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedSemanticallyVerified>,
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
    pub fn queue(&mut self, new: QueuedSemanticallyVerified) {
        let new_hash = new.0.hash;
        let new_height = new.0.height;
        let parent_hash = new.0.block.header.previous_block_hash;

        if self.blocks.contains_key(&new_hash) {
            // Skip queueing the block and return early if the hash is not unique
            return;
        }

        // Track known UTXOs in queued blocks.
        for (outpoint, ordered_utxo) in new.0.new_outputs.iter() {
            self.known_utxos
                .insert(*outpoint, ordered_utxo.utxo.clone());
        }

        self.blocks.insert(new_hash, new);
        self.by_height
            .entry(new_height)
            .or_default()
            .insert(new_hash);
        self.by_parent
            .entry(parent_hash)
            .or_default()
            .insert(new_hash);

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
    pub fn dequeue_children(
        &mut self,
        parent_hash: block::Hash,
    ) -> Vec<QueuedSemanticallyVerified> {
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
                QueueAndCommitError::new_pruned(expired_block.height).into()
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
    pub fn get_mut(&mut self, hash: &block::Hash) -> Option<&mut QueuedSemanticallyVerified> {
        self.blocks.get_mut(hash)
    }

    /// Update metrics after the queue is modified
    fn update_metrics(&self) {
        if let Some(min_height) = self.by_height.keys().next() {
            metrics::gauge!("state.memory.queued.min.height").set(min_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("state.memory.queued.min.height").set(f64::NAN);
        }
        if let Some(max_height) = self.by_height.keys().next_back() {
            metrics::gauge!("state.memory.queued.max.height").set(max_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("state.memory.queued.max.height").set(f64::NAN);
        }

        metrics::gauge!("state.memory.queued.block.count").set(self.blocks.len() as f64);
    }

    /// Try to look up this UTXO in any queued block.
    #[instrument(skip(self))]
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.known_utxos.get(outpoint).cloned()
    }

    /// Clears known_utxos, by_parent, and by_height, then drains blocks.
    /// Returns all key-value pairs of blocks as an iterator.
    ///
    /// Doesn't update the metrics, because it is only used when the state is being dropped.
    pub fn drain(&mut self) -> Drain<'_, block::Hash, QueuedSemanticallyVerified> {
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
    /// A list of previously sent block batches, each batch is in increasing height order.
    /// We use this list to efficiently prune outdated hashes that are at or below the finalized tip.
    bufs: Vec<VecDeque<(block::Hash, block::Height)>>,

    /// The list of blocks sent in the current batch, in increasing height order.
    curr_buf: VecDeque<(block::Hash, block::Height)>,

    /// Stores a set of hashes that have been sent to the block write task but
    /// may not be in the finalized state yet.
    pub sent: HashMap<block::Hash, Vec<transparent::OutPoint>>,

    /// Known UTXOs.
    known_utxos: HashMap<transparent::OutPoint, transparent::Utxo>,

    /// Whether the hashes in this struct can be used check if the chain can be forked.
    /// This is set to false until all checkpoint-verified block hashes have been pruned.
    pub(crate) can_fork_chain_at_hashes: bool,
}

impl SentHashes {
    /// Creates a new [`SentHashes`] with the block hashes and UTXOs in the provided non-finalized state.
    pub fn new(non_finalized_state: &NonFinalizedState) -> Self {
        let mut sent_hashes = Self::default();
        for (_, block) in non_finalized_state
            .chain_iter()
            .flat_map(|c| c.blocks.clone())
        {
            sent_hashes.add(&block.into());
        }

        if !sent_hashes.sent.is_empty() {
            sent_hashes.can_fork_chain_at_hashes = true;
        }

        sent_hashes
    }

    /// Stores the `block`'s hash, height, and UTXOs, so they can be used to check if a block or UTXO
    /// is available in the state.
    ///
    /// Assumes that blocks are added in the order of their height between `finish_batch` calls
    /// for efficient pruning.
    pub fn add(&mut self, block: &SemanticallyVerifiedBlock) {
        // Track known UTXOs in sent blocks.
        let outpoints = block
            .new_outputs
            .iter()
            .map(|(outpoint, ordered_utxo)| {
                self.known_utxos
                    .insert(*outpoint, ordered_utxo.utxo.clone());
                outpoint
            })
            .cloned()
            .collect();

        self.curr_buf.push_back((block.hash, block.height));
        self.sent.insert(block.hash, outpoints);

        self.update_metrics_for_block(block.height);
    }

    /// Stores the checkpoint verified `block`'s hash, height, and UTXOs, so they can be used to check if a
    /// block or UTXO is available in the state.
    ///
    /// Used for checkpoint verified blocks close to the final checkpoint, so the semantic block verifier can look up
    /// their UTXOs.
    ///
    /// Assumes that blocks are added in the order of their height between `finish_batch` calls
    /// for efficient pruning.
    ///
    /// For more details see `add()`.
    pub fn add_finalized(&mut self, block: &CheckpointVerifiedBlock) {
        // Track known UTXOs in sent blocks.
        let outpoints = block
            .new_outputs
            .iter()
            .map(|(outpoint, ordered_utxo)| {
                self.known_utxos
                    .insert(*outpoint, ordered_utxo.utxo.clone());
                outpoint
            })
            .cloned()
            .collect();

        self.curr_buf.push_back((block.hash, block.height));
        self.sent.insert(block.hash, outpoints);

        self.update_metrics_for_block(block.height);
    }

    /// Try to look up this UTXO in any sent block.
    #[instrument(skip(self))]
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.known_utxos.get(outpoint).cloned()
    }

    /// Finishes the current block batch, and stores it for efficient pruning.
    pub fn finish_batch(&mut self) {
        if !self.curr_buf.is_empty() {
            self.bufs.push(std::mem::take(&mut self.curr_buf));
        }
    }

    /// Prunes sent blocks at or below `height_bound`.
    ///
    /// Finishes the batch if `finish_batch()` hasn't been called already.
    ///
    /// Assumes that blocks will be added in order of their heights between each `finish_batch()` call,
    /// so that blocks can be efficiently and reliably removed by height.
    pub fn prune_by_height(&mut self, height_bound: block::Height) {
        self.finish_batch();

        // Iterates over each buf in `sent_bufs`, removing sent blocks until reaching
        // the first block with a height above the `height_bound`.
        self.bufs.retain_mut(|buf| {
            while let Some((hash, height)) = buf.pop_front() {
                if height > height_bound {
                    buf.push_front((hash, height));
                    return true;
                } else if let Some(expired_outpoints) = self.sent.remove(&hash) {
                    // TODO: only remove UTXOs if there are no queued blocks with that UTXO
                    //       (known_utxos is best-effort, so this is ok for now)
                    for outpoint in expired_outpoints.iter() {
                        self.known_utxos.remove(outpoint);
                    }
                }
            }

            false
        });

        self.sent.shrink_to_fit();
        self.known_utxos.shrink_to_fit();
        self.bufs.shrink_to_fit();

        self.update_metrics_for_cache();
    }

    /// Returns true if SentHashes contains the `hash`
    pub fn contains(&self, hash: &block::Hash) -> bool {
        self.sent.contains_key(hash)
    }

    /// Returns true if the chain can be forked at the provided hash
    pub fn can_fork_chain_at(&self, hash: &block::Hash) -> bool {
        self.can_fork_chain_at_hashes && self.contains(hash)
    }

    /// Update sent block metrics after a block is sent.
    fn update_metrics_for_block(&self, height: block::Height) {
        metrics::counter!("state.memory.sent.block.count").increment(1);
        metrics::gauge!("state.memory.sent.block.height").set(height.0 as f64);

        self.update_metrics_for_cache();
    }

    /// Update sent block cache metrics after the sent blocks are modified.
    fn update_metrics_for_cache(&self) {
        let batch_iter = || self.bufs.iter().chain(iter::once(&self.curr_buf));

        if let Some(min_height) = batch_iter()
            .flat_map(|batch| batch.front().map(|(_hash, height)| height))
            .min()
        {
            metrics::gauge!("state.memory.sent.cache.min.height").set(min_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("state.memory.sent.cache.min.height").set(f64::NAN);
        }

        if let Some(max_height) = batch_iter()
            .flat_map(|batch| batch.back().map(|(_hash, height)| height))
            .max()
        {
            metrics::gauge!("state.memory.sent.cache.max.height").set(max_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("state.memory.sent.cache.max.height").set(f64::NAN);
        }

        metrics::gauge!("state.memory.sent.cache.block.count")
            .set(batch_iter().flatten().count() as f64);

        metrics::gauge!("state.memory.sent.cache.batch.count").set(batch_iter().count() as f64);
    }
}
