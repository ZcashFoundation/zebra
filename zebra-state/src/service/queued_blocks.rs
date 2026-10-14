//! Queued blocks that are awaiting their parent block for verification.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    iter,
    mem::size_of,
};

use tokio::sync::oneshot;
use tracing::instrument;

use zebra_chain::{
    block, parameters::checkpoint::constants::MAX_CHECKPOINT_BYTE_COUNT, transparent,
};

use crate::{
    constants::{MAX_BLOCK_REORG_HEIGHT, MAX_NON_FINALIZED_CHAIN_FORKS},
    error::{CommitBlockError, CommitCheckpointVerifiedError},
    request::{block_retained_bytes, output_retained_bytes},
    CheckpointVerifiedBlock, CommitSemanticallyVerifiedError, ContextuallyVerifiedBlock,
    KnownBlock, MinedBlockNotifications, NonFinalizedState, SemanticallyVerifiedBlock,
};

#[cfg(test)]
mod tests;

/// A queued checkpoint-verified block, its result channel, and optional mined notifications.
pub type QueuedCheckpointVerified = (
    CheckpointVerifiedBlock,
    oneshot::Sender<Result<block::Hash, CommitCheckpointVerifiedError>>,
    Option<MinedBlockNotifications>,
);

/// A queued semantic block, its result channel, optional cached verdict, and mined notifications.
pub type QueuedSemanticallyVerified = (
    SemanticallyVerifiedBlock,
    oneshot::Sender<Result<block::Hash, CommitSemanticallyVerifiedError>>,
    // During queueing, the first field owns the cached result's new outputs.
    Option<Box<ContextuallyVerifiedBlock>>,
    Option<MinedBlockNotifications>,
);

/// One existing checkpoint download budget per supported non-finalized fork.
///
/// This 320 MiB resource charge covers retained content and decoded/index metadata, not allocator
/// RSS. Shared allocations are charged per owner, including cached prior-output scripts. Unlike a
/// single checkpoint budget, this allows ordinary concurrent full verification of large blocks.
// Both constants fit usize on every supported platform.
const MAX_QUEUED_BYTES: usize = MAX_CHECKPOINT_BYTE_COUNT as usize * MAX_NON_FINALIZED_CHAIN_FORKS;

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Debug, Default)]
pub struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, (QueuedSemanticallyVerified, usize)>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, HashSet<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, HashSet<block::Hash>>,
    /// Known UTXOs.
    known_utxos: HashMap<transparent::OutPoint, transparent::Utxo>,
    /// Sum of the immutable charges stored alongside queued payloads.
    queued_bytes: usize,
}

impl QueuedBlocks {
    /// Queue a block for eventual verification and commit.
    ///
    /// The caller must dispatch ready blocks, then call [`Self::prune_overflow`] before returning.
    /// The queue can temporarily exceed its limits by this one incoming block, without awaiting.
    /// This lets a commit-ready parent release a full queue of children before applying the limits.
    #[instrument(skip(self), fields(height = ?new.0.height, hash = %new.0.hash))]
    pub fn queue(&mut self, new: QueuedSemanticallyVerified) {
        let new_hash = new.0.hash;
        let new_height = new.0.height;
        let parent_hash = new.0.block.header.previous_block_hash;

        if self.blocks.contains_key(&new_hash) {
            // Skip queueing the block and return early if the hash is not unique
            return;
        }

        let bytes = Self::retained_bytes(&new);
        let Some(queued_bytes) = self
            .queued_bytes
            .checked_add(bytes)
            .filter(|_| bytes < usize::MAX)
        else {
            let _ = new.1.send(Err(CommitBlockError::OrphanQueueFull.into()));
            return;
        };

        // Track known UTXOs in queued blocks.
        for (outpoint, ordered_utxo) in new.0.new_outputs.iter() {
            self.known_utxos
                .insert(*outpoint, ordered_utxo.utxo.clone());
        }

        self.queued_bytes = queued_bytes;
        self.blocks.insert(new_hash, (new, bytes));
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

    /// Conservative content/metadata charge, computed once without copying transactions or outputs.
    fn retained_bytes(queued: &QueuedSemanticallyVerified) -> usize {
        let prepared = &queued.0;
        let mut bytes = block_retained_bytes(&prepared.block, &prepared.transaction_hashes)
            .saturating_add(size_of::<(QueuedSemanticallyVerified, usize, block::Hash)>())
            .saturating_add(2 * size_of::<(block::Hash, HashSet<block::Hash>)>());
        // Charge the semantic output map and its best-effort UTXO-cache copy independently.
        bytes =
            bytes.saturating_add(output_retained_bytes(&prepared.new_outputs).saturating_mul(2));
        if let Some(contextual) = &queued.2 {
            bytes = bytes.saturating_add(contextual.contextual_data_retained_bytes());
        }
        bytes
    }

    /// Drop highest-height orphans until both resource limits hold, after ready work is dispatched.
    ///
    /// Pressure is an operational error, not evidence that the evicted block is invalid.
    pub fn prune_overflow(&mut self) {
        // The reorganization window fits usize on every supported platform.
        while self.blocks.len() > MAX_BLOCK_REORG_HEIGHT as usize
            || self.queued_bytes > MAX_QUEUED_BYTES
        {
            let hash = *self
                .by_height
                .last_key_value()
                .and_then(|(_, hashes)| hashes.iter().next())
                .expect("an over-limit queue contains an indexed block");
            let (_, sender, _, _) = self.remove(&hash);
            let _ = sender.send(Err(CommitBlockError::OrphanQueueFull.into()));
        }
        self.update_metrics();
    }

    /// Remove one payload, its charge, and every secondary reference.
    fn remove(&mut self, hash: &block::Hash) -> QueuedSemanticallyVerified {
        let (queued, bytes) = self.blocks.remove(hash).expect("indexed block is queued");
        self.queued_bytes = self
            .queued_bytes
            .checked_sub(bytes)
            .expect("each queued payload is charged and removed exactly once");
        let parent = queued.0.block.header.previous_block_hash;
        // Dequeue and height pruning can already have removed the corresponding whole bucket.
        if let Some(hashes) = self.by_parent.get_mut(&parent) {
            assert!(hashes.remove(hash), "queued block is indexed by parent");
            if hashes.is_empty() {
                self.by_parent.remove(&parent);
            }
        }
        if let Some(hashes) = self.by_height.get_mut(&queued.0.height) {
            assert!(hashes.remove(hash), "queued block is indexed by height");
            if hashes.is_empty() {
                self.by_height.remove(&queued.0.height);
            }
        }
        // This cache remains best-effort: another queued fork can contain the same output.
        for outpoint in queued.0.new_outputs.keys() {
            self.known_utxos.remove(outpoint);
        }
        queued
    }

    /// Returns `true` if there are any queued children of `parent_hash`.
    #[instrument(skip(self), fields(%parent_hash))]
    pub fn has_queued_children(&self, parent_hash: block::Hash) -> bool {
        self.by_parent.contains_key(&parent_hash)
    }

    /// Returns `true` if there is a block with the provided hash present.
    #[instrument(skip(self), fields(%hash))]
    pub fn has(&self, hash: block::Hash) -> bool {
        self.blocks.contains_key(&hash)
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
            .map(|hash| self.remove(&hash))
            .collect::<Vec<_>>();

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
        while let Some((&height, _)) = self.by_height.first_key_value() {
            if height > finalized_tip_height {
                break;
            }
            let (_, hashes) = self.by_height.pop_first().expect("first height exists");
            for hash in hashes {
                let (expired_block, expired_sender, _, _) = self.remove(&hash);
                // Receiver closure does not cancel accepted work.
                let _ = expired_sender.send(Err(CommitBlockError::new_duplicate(
                    Some(expired_block.height.into()),
                    KnownBlock::Finalized,
                )
                .into()));
            }
        }

        tracing::trace!(num_blocks = %self.blocks.len(), "Finished pruning blocks at or beneath the finalized tip height");
        self.update_metrics();
    }

    /// Return the queued block if it has already been registered
    pub fn get_mut(&mut self, hash: &block::Hash) -> Option<&mut QueuedSemanticallyVerified> {
        self.blocks.get_mut(hash).map(|(queued, _)| queued)
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
    pub fn drain(
        &mut self,
    ) -> impl Iterator<Item = (block::Hash, QueuedSemanticallyVerified)> + '_ {
        self.known_utxos.clear();
        self.known_utxos.shrink_to_fit();
        self.by_parent.clear();
        self.by_parent.shrink_to_fit();
        self.by_height.clear();

        self.queued_bytes = 0;
        self.blocks
            .drain()
            .map(|(hash, (queued, _))| (hash, queued))
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

    /// Removes a `hash` from `SentHashes`, dropping its outpoints from `known_utxos`
    /// and its entry from whichever batch buffer holds it.
    ///
    /// Called when the block write task rejects a block, so that a subsequent
    /// re-delivery of a block with the same hash is not short-circuited as a
    /// "duplicate" against a rejected variant that never reached any chain.
    pub fn remove(&mut self, hash: &block::Hash) {
        let Some(outpoints) = self.sent.remove(hash) else {
            return;
        };

        for outpoint in &outpoints {
            self.known_utxos.remove(outpoint);
        }

        self.curr_buf.retain(|(h, _)| h != hash);
        for buf in &mut self.bufs {
            buf.retain(|(h, _)| h != hash);
        }
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
