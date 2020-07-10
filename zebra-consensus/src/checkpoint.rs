//! Checkpoint-based block verification for Zebra.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to speed up the
//! initial chain sync for Zebra. This list is distributed with Zebra.
//!
//! The CheckpointVerifier queues pending blocks. Once there is a chain from the
//! previous checkpoint to a target checkpoint, it verifies all the blocks in
//! that chain.
//!
//! Verification starts at the first checkpoint, which is the genesis block for the
//! configured network.
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

mod list;
mod types;

#[cfg(test)]
mod tests;

use list::CheckpointList;
use types::{Progress, Progress::*};
use types::{Target, Target::*};

use futures_util::FutureExt;
use std::{
    collections::BTreeMap,
    error,
    future::Future,
    ops::{Bound, Bound::*},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::oneshot;
use tower::Service;

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

/// The inner error type for CheckpointVerifier.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// An unverified block, which is in the queue for checkpoint verification.
#[derive(Debug)]
struct QueuedBlock {
    /// The block data.
    block: Arc<Block>,
    /// `block`'s cached header hash.
    hash: BlockHeaderHash,
    /// The transmitting end of the oneshot channel for this block's result.
    tx: oneshot::Sender<Result<BlockHeaderHash, Error>>,
}

/// A list of unverified blocks at a particular height.
///
/// Typically contains zero or one blocks, but might contain more if a peer
/// has an old chain fork. (Or sends us a bad block.)
type QueuedBlockList = Vec<QueuedBlock>;

/// A checkpointing block verifier.
///
/// Verifies blocks using a supplied list of checkpoints. There must be at
/// least one checkpoint for the genesis block.
#[derive(Debug)]
struct CheckpointVerifier {
    // Inputs
    //
    /// The checkpoint list for this verifier.
    checkpoint_list: CheckpointList,

    // Queued Blocks
    //
    /// A queue of unverified blocks.
    ///
    /// Contains a list of unverified blocks at each block height. In most cases,
    /// the checkpoint verifier will store zero or one block at each height.
    ///
    /// Blocks are verified in order, once there is a chain from the previous
    /// checkpoint to a target checkpoint.
    ///
    /// The first checkpoint does not have any ancestors, so it only verifies the
    /// genesis block.
    queued: BTreeMap<BlockHeight, QueuedBlockList>,

    /// The current progress of this verifier.
    verifier_progress: Progress<BlockHeight>,
}

/// The CheckpointVerifier implementation.
///
/// Contains non-service utility functions for CheckpointVerifiers.
impl CheckpointVerifier {
    /// Return a checkpoint verification service, using the provided `checkpoint_list`.
    ///
    /// This function should be called only once for a particular checkpoint list (and
    /// network), rather than constructing multiple verification services based on the
    /// same checkpoint list. To Clone a CheckpointVerifier, you might need to wrap it
    /// in a `tower::Buffer` service.
    //
    // Avoid some dead code lints.
    // Until we implement the overall verifier in #516, this function, and some of the
    // functions and enum variants it uses, are only used in the tests.
    #[allow(dead_code)]
    fn new(
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        Ok(CheckpointVerifier {
            checkpoint_list: CheckpointList::new(checkpoint_list)?,
            queued: BTreeMap::new(),
            // We start by verifying the genesis block, by itself
            verifier_progress: Progress::BeforeGenesis,
        })
    }

    /// Return the current verifier's progress.
    ///
    /// If verification has not started yet, returns `BeforeGenesis`.
    ///
    /// If verification is ongoing, returns `PreviousCheckpoint(height)`.
    /// `height` increases as checkpoints are verified.
    ///
    /// If verification has finished, returns `FinalCheckpoint`.
    fn previous_checkpoint_height(&self) -> Progress<BlockHeight> {
        self.verifier_progress
    }

    /// Return the start of the current checkpoint range.
    ///
    /// Returns None if verification has finished.
    fn current_start_bound(&self) -> Option<Bound<BlockHeight>> {
        match self.previous_checkpoint_height() {
            BeforeGenesis => Some(Unbounded),
            PreviousCheckpoint(height) => Some(Excluded(height)),
            FinalCheckpoint => None,
        }
    }

    /// Return the target checkpoint height that we want to verify.
    ///
    /// If we need more blocks, returns `WaitingForBlocks`.
    ///
    /// If the queued blocks are continuous from the previous checkpoint to a
    /// target checkpoint, returns `Checkpoint(height)`. The target checkpoint
    /// can be multiple checkpoints ahead of the previous checkpoint.
    ///
    /// `height` increases as checkpoints are verified.
    ///
    /// If verification has finished, returns `FinishedVerifying`.
    fn target_checkpoint_height(&self) -> Target<BlockHeight> {
        // Find the height we want to start searching at
        let mut pending_height = match self.previous_checkpoint_height() {
            // Check if we have the genesis block as a special case, to simplify the loop
            BeforeGenesis if !self.queued.contains_key(&BlockHeight(0)) => return WaitingForBlocks,
            BeforeGenesis => BlockHeight(0),
            PreviousCheckpoint(height) => height,
            FinalCheckpoint => return FinishedVerifying,
        };

        // Find the end of the continuous sequence of blocks, starting at the
        // last verified checkpoint. If there is no verified checkpoint, start
        // *after* the genesis block (which we checked above).
        //
        // If `btree_map::Range` implements `ExactSizeIterator`, it would be
        // much faster to walk the checkpoint list, and compare the length of
        // the `btree_map::Range` to the block height difference between
        // checkpoints. (In maps, keys are unique, so we don't need to check
        // each height value.)
        //
        // But at the moment, this implementation is slightly faster, because
        // it stops after the first gap.
        for (&height, _) in self.queued.range((Excluded(pending_height), Unbounded)) {
            // If the queued blocks are continuous.
            if height == BlockHeight(pending_height.0 + 1) {
                pending_height = height;
            } else {
                break;
            }
        }

        // Now find the start of the checkpoint range
        let start = self.current_start_bound().expect(
            "if verification has finished, we should have returned earlier in the function",
        );
        // Find the highest checkpoint below pending_height, excluding any
        // previously verified checkpoints
        let target_checkpoint = self
            .checkpoint_list
            .max_height_in_range((start, Included(pending_height)));

        target_checkpoint
            .map(Checkpoint)
            .unwrap_or(WaitingForBlocks)
    }

    /// Return the most recently verified checkpoint's hash.
    ///
    /// See `previous_checkpoint_height()` for details.
    fn previous_checkpoint_hash(&self) -> Progress<BlockHeaderHash> {
        match self.previous_checkpoint_height() {
            BeforeGenesis => BeforeGenesis,
            PreviousCheckpoint(height) => self
                .checkpoint_list
                .hash(height)
                .map(PreviousCheckpoint)
                .expect("every checkpoint height must have a hash"),
            FinalCheckpoint => FinalCheckpoint,
        }
    }

    /// Check that `height` is valid and able to be verified.
    ///
    /// Returns an error if:
    ///  - the block's height is greater than the maximum checkpoint
    ///  - there are no checkpoints
    ///  - the block's height is less than or equal to the previously verified
    ///    checkpoint
    ///  - verification has finished
    fn check_height(&self, height: BlockHeight) -> Result<(), Error> {
        if height > self.checkpoint_list.max_height() {
            Err("block is higher than the maximum checkpoint")?;
        }

        match self.previous_checkpoint_height() {
            // Any height is valid
            BeforeGenesis => {}
            // Greater heights are valid
            PreviousCheckpoint(previous_height) if (height <= previous_height) => {
                Err("block height has already been verified")?
            }
            PreviousCheckpoint(_) => {}
            // We're finished, so no checkpoint height is valid
            FinalCheckpoint => Err("verification has finished")?,
        };

        Ok(())
    }

    /// Increase the current checkpoint height to `verified_height`,
    fn update_progress(&mut self, verified_height: BlockHeight) {
        // Ignore blocks that are below the previous checkpoint, or otherwise
        // have invalid heights.
        //
        // We ignore out-of-order verification, such as:
        //  - the height is less than the previous checkpoint height, or
        //  - the previous checkpoint height is the maximum height (checkpoint verifies are finished),
        // because futures might not resolve in height order.
        if self.check_height(verified_height).is_err() {
            return;
        }

        // Ignore heights that aren't checkpoint heights
        if verified_height == self.checkpoint_list.max_height() {
            self.verifier_progress = FinalCheckpoint;
        } else if self.checkpoint_list.contains(verified_height) {
            self.verifier_progress = PreviousCheckpoint(verified_height);
        }
    }

    /// If the block height of `block` is valid, returns that height.
    ///
    /// Returns an error if the block's height is invalid, see `check_height()`
    /// for details.
    fn check_block(&self, block: &Block) -> Result<BlockHeight, Error> {
        let block_height = block
            .coinbase_height()
            .ok_or("the block does not have a coinbase height")?;
        self.check_height(block_height)?;
        Ok(block_height)
    }

    /// Queue `block` for verification, and return the `Receiver` for the
    /// block's verification result.
    ///
    /// Verification will finish when the chain to the next checkpoint is
    /// complete, and the caller will be notified via the channel.
    ///
    /// If the block does not have a coinbase height, sends an error on `tx`,
    /// and does not queue the block.
    fn queue_block(
        &mut self,
        block: Arc<Block>,
    ) -> oneshot::Receiver<Result<BlockHeaderHash, Error>> {
        // Set up a oneshot channel to send results
        let (tx, rx) = oneshot::channel();

        // Check for a valid height
        let height = match self.check_block(&block) {
            Ok(height) => height,
            Err(error) => {
                // Sending might fail, depending on what the caller does with rx,
                // but there's nothing we can do about it.
                let _ = tx.send(Err(error));
                return rx;
            }
        };

        // Add the block to the list of queued blocks at this height
        let hash = block.as_ref().into();
        let new_qblock = QueuedBlock { block, hash, tx };
        self.queued.entry(height).or_default().push(new_qblock);

        rx
    }

    /// During checkpoint range processing, process all the blocks at `height`.
    ///
    /// Returns the first valid block. If there is no valid block, returns None.
    fn process_height(
        &mut self,
        height: BlockHeight,
        expected_hash: BlockHeaderHash,
    ) -> Option<QueuedBlock> {
        let mut qblocks = self
            .queued
            .remove(&height)
            .expect("the current checkpoint range has continuous Vec<QueuedBlock>s");
        assert!(
            !qblocks.is_empty(),
            "the current checkpoint range has continous Blocks"
        );

        // Check interim checkpoints
        if let Some(checkpoint_hash) = self.checkpoint_list.hash(height) {
            // We assume the checkpoints are valid. And we have verified back
            // from the target checkpoint, so the last block must also be valid.
            // This is probably a bad checkpoint list, a zebra bug, or a bad
            // chain (in a testing mode like regtest).
            assert_eq!(expected_hash, checkpoint_hash,
                           "checkpoints in the range should match: bad checkpoint list, zebra bug, or bad chain"
                );
        }

        // Find a queued block at this height, which is part of the hash chain.
        //
        // There are two possible outcomes here:
        //   - at least one block matches the chain (the common case)
        //     (if there are duplicate blocks, one succeeds, and the others fail)
        //   - no blocks match the chain, verification has failed for this range
        let mut valid_qblock = None;
        for qblock in qblocks.drain(..) {
            if qblock.hash == expected_hash {
                if valid_qblock.is_none() {
                    // The first valid block at the current height
                    valid_qblock = Some(qblock);
                } else {
                    // Reject duplicate blocks at the same height
                    let _ = qblock.tx.send(Err(
                        "duplicate valid blocks at this height, only one was chosen".into(),
                    ));
                }
            } else {
                // A bad block, that isn't part of the chain.
                let _ = qblock.tx.send(Err(
                    "the block hash does not match the chained checkpoint hash".into(),
                ));
            }
        }

        valid_qblock
    }

    /// Check all the blocks in the current checkpoint range.
    ///
    /// Send `Ok` for the blocks that are in the chain, and `Err` for side-chain
    /// blocks.
    ///
    /// Does nothing if we are waiting for more blocks, or if verification has
    /// finished.
    fn process_checkpoint_range(&mut self) {
        // If this code shows up in profiles, we can try the following
        // optimisations:
        //   - only check the chain when the length of the queue is greater
        //     than or equal to the length of a checkpoint interval
        //     (note: the genesis checkpoint interval is only one block long)
        //   - cache the height of the last continuous chain as a new field in
        //     self, and start at that height during the next check.

        // Return early if verification has finished
        let previous_checkpoint_hash = match self.previous_checkpoint_hash() {
            // Since genesis blocks are hard-coded in zcashd, and not verified
            // like other blocks, the genesis parent hash is set by the
            // consensus parameters.
            //
            // TODO(teor): get the genesis block parent hash from the consensus
            //             parameters
            // In the meantime, try `[0; 32])`, because the genesis block has no
            // parent block. (And in Bitcoin, `null` is `[0; 32]`.)
            BeforeGenesis => BlockHeaderHash([0; 32]),
            PreviousCheckpoint(hash) => hash,
            FinalCheckpoint => return,
        };
        // Return early if we're still waiting for more blocks
        let (target_checkpoint_height, mut expected_hash) = match self.target_checkpoint_height() {
            Checkpoint(height) => (
                height,
                self.checkpoint_list
                    .hash(height)
                    .expect("every checkpoint height must have a hash"),
            ),
            _ => return,
        };

        // Keep the old previous checkpoint height, to make sure we're making
        // progress
        let old_prev_check_height = self.previous_checkpoint_height();

        // Work out which blocks and checkpoints we're checking
        let current_range = (
            self.current_start_bound()
                .expect("earlier code checks if verification has finished"),
            Included(target_checkpoint_height),
        );
        let range_heights: Vec<BlockHeight> = self
            .queued
            .range_mut(current_range)
            .rev()
            .map(|(key, _)| *key)
            .collect();
        // A list of pending valid blocks, in reverse chain order
        let mut rev_valid_blocks = Vec::new();

        // Check all the blocks, and discard all the bad blocks
        for current_height in range_heights {
            let valid_qblock = self.process_height(current_height, expected_hash);
            if let Some(qblock) = valid_qblock {
                expected_hash = qblock.block.header.previous_block_hash;
                // Add the block to the end of the pending block list
                // (since we're walking the chain backwards, the list is
                // in reverse chain order)
                rev_valid_blocks.push(qblock);
            } else {
                // The last block height we processed did not have any blocks
                // with a matching hash, so chain verification has failed.
                //
                // TODO(teor||jlusby): log an error here?

                // We kept all the matching blocks down to this height, in
                // anticipation of the chain verifying. But the chain is
                // incomplete, so we have to put them back in the queue.
                //
                // The order here shouldn't matter, but add the blocks in
                // height order, for consistency.
                for vblock in rev_valid_blocks.drain(..).rev() {
                    let height = vblock
                        .block
                        .coinbase_height()
                        .expect("queued blocks have a block height");
                    self.queued.entry(height).or_default().push(vblock);
                }

                // Make sure the current progress hasn't changed
                assert_eq!(
                    self.previous_checkpoint_height(),
                    old_prev_check_height,
                    "we must not change the previous checkpoint on failure"
                );
                // We've reduced the target
                //
                // This check should be cheap, because we just reduced the target
                let current_target = self.target_checkpoint_height();
                assert!(
                    current_target == WaitingForBlocks
                        || current_target < Checkpoint(target_checkpoint_height),
                    "we must decrease or eliminate our target on failure"
                );

                // Stop verifying, and wait for the next valid block
                return;
            }
        }

        // The checkpoint and the parent hash must match.
        // See the detailed checkpoint comparison comment above.
        assert_eq!(
            expected_hash, previous_checkpoint_hash,
            "the previous checkpoint should match: bad checkpoint list, zebra bug, or bad chain"
        );

        // All the blocks we've kept are valid, so let's verify them
        // in height order.
        for qblock in rev_valid_blocks.drain(..).rev() {
            // Sending can fail, but there's nothing we can do about it.
            let _ = qblock.tx.send(Ok(qblock.hash));
        }

        // Finally, update the checkpoint bounds
        self.update_progress(target_checkpoint_height);

        // Ensure that we're making progress
        let new_progress = self.previous_checkpoint_height();
        assert!(
            new_progress > old_prev_check_height,
            "we must make progress on success"
        );
        // We met the old target
        if new_progress == FinalCheckpoint {
            assert_eq!(
                target_checkpoint_height,
                self.checkpoint_list.max_height(),
                "we finish at the maximum checkpoint"
            );
        } else {
            assert_eq!(
                new_progress,
                PreviousCheckpoint(target_checkpoint_height),
                "the new previous checkpoint must match the old target"
            );
        }
        // We processed all available checkpoints
        //
        // We've cleared the target range, so this check should be cheap
        let new_target = self.target_checkpoint_height();
        assert!(
            new_target == WaitingForBlocks || new_target == FinishedVerifying,
            "processing must cover all available checkpoints"
        );
    }
}

/// CheckpointVerifier rejects pending futures on drop.
impl Drop for CheckpointVerifier {
    /// Send an error on `tx` for any `QueuedBlock`s that haven't been verified.
    ///
    /// We can't implement `Drop` on QueuedBlock, because `send()` consumes
    /// `tx`. And `tx` doesn't implement `Copy` or `Default` (for `take()`).
    fn drop(&mut self) {
        let drop_keys: Vec<_> = self.queued.keys().cloned().collect();
        for key in drop_keys {
            let mut qblocks = self
                .queued
                .remove(&key)
                .expect("each entry is only removed once");
            for qblock in qblocks.drain(..) {
                // Sending can fail, but there's nothing we can do about it.
                let _ = qblock
                    .tx
                    .send(Err("checkpoint verifier was dropped".into()));
            }
        }
    }
}

/// The CheckpointVerifier service implementation.
///
/// After verification, the block futures resolve to their hashes.
impl Service<Arc<Block>> for CheckpointVerifier {
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.previous_checkpoint_height() {
            FinalCheckpoint => Poll::Ready(Err("there are no checkpoints left to verify".into())),
            _ => Poll::Ready(Ok(())),
        }
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report

        // Queue the block for verification, until we receive all the blocks for
        // the current checkpoint range.
        let rx = self.queue_block(block);

        // Try to verify from the previous checkpoint to a target checkpoint.
        //
        // If there are multiple checkpoints in the target range, and one of
        // the ranges is invalid, we'll try again with a smaller target range
        // on the next call(). Failures always reject a block, so we know
        // there will be at least one more call().
        //
        // TODO(teor): retry on failure (low priority, failures should be rare)
        self.process_checkpoint_range();

        async move {
            // Remove the Result<..., RecvError> wrapper from the channel future
            rx.await
                .expect("CheckpointVerifier does not leave dangling receivers")
        }
        .boxed()
    }
}
