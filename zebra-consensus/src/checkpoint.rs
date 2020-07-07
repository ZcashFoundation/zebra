//! Checkpoint-based block verification for Zebra.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to speed up the
//! initial chain sync for Zebra. This list is distributed with Zebra.
//!
//! The CheckpointVerifier queues pending blocks. Once there is a chain between
//! the next pair of checkpoints, it verifies all the blocks in that chain.
//! Verification starts at the first checkpoint, which is the genesis block for the
//! configured network.
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use futures_util::FutureExt;
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    error,
    future::Future,
    ops::{Bound, Bound::*, RangeBounds},
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

/// A `CheckpointVerifier`'s current progress verifying the chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Progress<HeightOrHash> {
    /// We have not verified any blocks yet.
    // rustc is smart enough to know that this variant isn't used for hashes,
    // because we always check the height before getting the hash.
    #[allow(dead_code)]
    BeforeGenesis,
    /// We have verified up to and including this checkpoint.
    PreviousCheckpoint(HeightOrHash),
    /// We have finished verifying.
    ///
    /// The final checkpoint is not included in this variant. The verifier has
    /// finished, so the checkpoints aren't particularly useful.
    /// To get the value of the final checkpoint, use `max_checkpoint_height()`.
    FinalCheckpoint,
}

/// Block height progress, in chain order.
impl Ord for Progress<BlockHeight> {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }
        match (self, other) {
            (BeforeGenesis, _) => Ordering::Less,
            (_, BeforeGenesis) => Ordering::Greater,
            (FinalCheckpoint, _) => Ordering::Greater,
            (_, FinalCheckpoint) => Ordering::Less,
            (PreviousCheckpoint(self_height), PreviousCheckpoint(other_height)) => {
                self_height.cmp(other_height)
            }
        }
    }
}

/// The partial ordering must match the total ordering.
impl PartialOrd for Progress<BlockHeight> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A `CheckpointVerifier`'s target checkpoint, based on the current queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target<HeightOrHash> {
    /// We need more blocks before we can choose a target checkpoint.
    WaitingForBlocks,
    /// We want to verify this checkpoint.
    ///
    /// The target checkpoint can be multiple checkpoints ahead of the previous
    /// checkpoint.
    Checkpoint(HeightOrHash),
    /// We have finished verifying, there will be no more targets.
    FinishedVerifying,
}

/// Block height target, in chain order.
///
/// `WaitingForBlocks` is incomparable with itself and `Checkpoint(_)`.
impl PartialOrd for Target<BlockHeight> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (FinishedVerifying, FinishedVerifying) => Some(Ordering::Equal),
            (FinishedVerifying, _) => Some(Ordering::Greater),
            (_, FinishedVerifying) => Some(Ordering::Less),
            (Checkpoint(self_height), Checkpoint(other_height)) => {
                self_height.partial_cmp(other_height)
            }
            // We can wait for blocks before or after any target checkpoint.
            // WaitingForBlocks is also incomparable with itself.
            _ => None,
        }
    }
}

use Progress::*;
use Target::*;

/// Each checkpoint consists of a coinbase height and block header hash.
///
/// Checkpoints should be chosen to avoid forks or chain reorganizations,
/// which only happen in the last few hundred blocks in the chain.
/// (zcashd allows chain reorganizations up to 99 blocks, and prunes
/// orphaned side-chains after 288 blocks.)
///
/// There must be a checkpoint for the genesis block at BlockHeight 0.
/// (All other checkpoints are optional.)
#[derive(Debug)]
struct CheckpointList(BTreeMap<BlockHeight, BlockHeaderHash>);

impl CheckpointList {
    /// Create a new checkpoint list from `checkpoint_list`.
    //
    // Right now, we only use this function in the tests.
    #[cfg(test)]
    fn new(
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_list.into_iter().collect();

        // An empty checkpoint list can't actually verify any blocks.
        match checkpoints.keys().cloned().next() {
            Some(BlockHeight(0)) => {}
            None => Err("there must be at least one checkpoint, for the genesis block")?,
            _ => Err("checkpoints must start at the genesis block height 0")?,
        };

        Ok(CheckpointList(checkpoints))
    }

    /// Is there a checkpoint at `height`?
    ///
    /// See `BTreeMap::contains_key()` for details.
    fn contains(&self, height: &BlockHeight) -> bool {
        self.0.contains_key(height)
    }

    /// Returns the hash corresponding to the checkpoint at `height`,
    /// or None if there is no checkpoint at that height.
    ///
    /// See `BTreeMap::get()` for details.
    fn hash(&self, height: &BlockHeight) -> Option<&BlockHeaderHash> {
        self.0.get(height)
    }

    /// Return the block height of the highest checkpoint in the checkpoint list.
    ///
    /// If there is only a single checkpoint, then the maximum height will be
    /// zero. (The genesis block.)
    ///
    /// The maximum height is constant for each checkpoint list.
    fn max_height(&self) -> BlockHeight {
        self.0
            .keys()
            .cloned()
            .next_back()
            .expect("checkpoint lists must have at least one checkpoint")
    }

    /// Return the block height of the highest checkpoint in a sub-range.
    fn max_height_in_range<R>(&self, range: R) -> Option<BlockHeight>
    where
        R: RangeBounds<BlockHeight>,
    {
        self.0.range(range).map(|(height, _)| *height).next_back()
    }
}

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
    /// Blocks are verified in order, when there is a chain from a subsequent
    /// checkpoint, back to the previous checkpoint. Each checkpoint range is
    /// used to verify all the blocks between the previous and target
    /// checkpoints.
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
    // Currently only used in tests.
    //
    // We'll use this function in the overall verifier, which will split blocks
    // between BlockVerifier and CheckpointVerifier.
    #[cfg(test)]
    fn new(
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        Ok(CheckpointVerifier {
            checkpoint_list: CheckpointList::new(checkpoint_list)?,
            queued: <BTreeMap<BlockHeight, QueuedBlockList>>::new(),
            // We start by verifying the genesis block, by itself
            verifier_progress: Progress::BeforeGenesis,
        })
    }

    /// Return the block height of the highest checkpoint in the checkpoint list.
    ///
    /// If there is only a single checkpoint, then the maximum height will be
    /// zero. (The genesis block.)
    ///
    /// The maximum height is constant for each checkpoint list.
    fn max_checkpoint_height(&self) -> BlockHeight {
        self.checkpoint_list.max_height()
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
    /// subsequent checkpoint, returns `Checkpoint(height)`. The target
    /// checkpoint can be multiple checkpoints ahead of the previous checkpoint.
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
            //
            // Precondition: queued block heights are within the valid
            // BlockHeight range, so there is no integer overflow.
            assert!(
                pending_height.0 < u32::MAX,
                "pending block had a height of u32::MAX "
            );
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

        if let Some(target_checkpoint) = target_checkpoint {
            Checkpoint(target_checkpoint)
        } else {
            WaitingForBlocks
        }
    }

    /// Return the most recently verified checkpoint's hash.
    ///
    /// See `previous_checkpoint_height()` for details.
    fn previous_checkpoint_hash(&self) -> Progress<BlockHeaderHash> {
        match self.previous_checkpoint_height() {
            BeforeGenesis => BeforeGenesis,
            PreviousCheckpoint(height) => self
                .checkpoint_list
                .hash(&height)
                .map(|hash| PreviousCheckpoint(*hash))
                .expect("every checkpoint height must have a hash"),
            FinalCheckpoint => FinalCheckpoint,
        }
    }

    /// Return the hash of the next checkpoint we want to verify.
    ///
    /// See `target_checkpoint_height()` for details.
    fn target_checkpoint_hash(&self) -> Target<BlockHeaderHash> {
        match self.target_checkpoint_height() {
            WaitingForBlocks => WaitingForBlocks,
            Checkpoint(height) => self
                .checkpoint_list
                .hash(&height)
                .map(|hash| Checkpoint(*hash))
                .expect("every checkpoint height must have a hash"),
            FinishedVerifying => FinishedVerifying,
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
        if height > self.max_checkpoint_height() {
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
        if verified_height == self.max_checkpoint_height() {
            self.verifier_progress = FinalCheckpoint;
        } else if self.checkpoint_list.contains(&verified_height) {
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
        if let Some(checkpoint_hash) = self.checkpoint_list.hash(&height) {
            // We assume the checkpoints are valid. And we have verified back
            // from the target checkpoint, so the last block must also be valid.
            // This is probably a bad checkpoint list, a zebra bug, or a bad
            // chain (in a testing mode like regtest).
            assert_eq!(expected_hash, *checkpoint_hash,
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
        let mut expected_hash = match self.target_checkpoint_hash() {
            Checkpoint(hash) => hash,
            _ => return,
        };
        let target_checkpoint_height = match self.target_checkpoint_height() {
            Checkpoint(height) => height,
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

                // Ensure that we're making progress
                assert_eq!(
                    self.previous_checkpoint_height(),
                    old_prev_check_height,
                    "processing must not change progress on failure"
                );
                let current_target = self.target_checkpoint_height();
                assert!(
                    current_target == WaitingForBlocks
                        || current_target < Checkpoint(target_checkpoint_height),
                    "processing must decrease or eliminate target on failure"
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
        assert!(
            self.previous_checkpoint_height() > old_prev_check_height,
            "progress must increase on success"
        );
        // We've cleared the target range, so this check should be cheap
        let current_target = self.target_checkpoint_height();
        assert!(
            current_target == WaitingForBlocks || current_target == FinishedVerifying,
            "processing must clear target on success"
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

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{eyre, Report};
    use std::{cmp::min, mem::drop, time::Duration};
    use tokio::time::timeout;
    use tower::{Service, ServiceExt};

    use zebra_chain::serialization::ZcashDeserialize;

    /// The timeout we apply to each verify future during testing.
    ///
    /// The checkpoint verifier uses `tokio::sync::oneshot` channels as futures.
    /// If the verifier doesn't send a message on the channel, any tests that
    /// await the channel future will hang.
    ///
    /// This value is set to a large value, to avoid spurious failures due to
    /// high system load.
    const VERIFY_TIMEOUT_SECONDS: u64 = 10;

    #[tokio::test]
    #[spandoc::spandoc]
    async fn single_item_checkpoint_list() -> Result<(), Report> {
        zebra_test::init();

        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let hash0: BlockHeaderHash = block0.as_ref().into();

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), hash0)]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for block 0
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block0.clone()),
        );
        /// Wait for the response for block 0
        // TODO(teor || jlusby): check error kind
        let verify_response = verify_future
            .await
            .expect("timeout should not happen")
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, hash0);

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn multi_item_checkpoint_list() -> Result<(), Report> {
        zebra_test::init();

        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            // TODO(teor): not continuous, so they hang
            //&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            //&zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((block.clone(), block.coinbase_height().unwrap(), hash));
        }

        // Make a checkpoint list containing all the blocks
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> = checkpoint_data
            .iter()
            .map(|(_block, height, hash)| (*height, *hash))
            .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(1));

        // Now verify each block
        for (block, height, hash) in checkpoint_data {
            /// Make sure the verifier service is ready
            let ready_verifier_service = checkpoint_verifier
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?;

            /// Set up the future for block {?height}
            let verify_future = timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                ready_verifier_service.call(block.clone()),
            );
            /// Wait for the response for block {?height}
            // TODO(teor || jlusby): check error kind
            let verify_response = verify_future
                .await
                .expect("timeout should not happen")
                .map_err(|e| eyre!(e))?;

            assert_eq!(verify_response, hash);

            if height < checkpoint_verifier.max_checkpoint_height() {
                assert_eq!(
                    checkpoint_verifier.previous_checkpoint_height(),
                    PreviousCheckpoint(height)
                );
                assert_eq!(
                    checkpoint_verifier.target_checkpoint_height(),
                    WaitingForBlocks
                );
            } else {
                assert_eq!(
                    checkpoint_verifier.previous_checkpoint_height(),
                    FinalCheckpoint
                );
                assert_eq!(
                    checkpoint_verifier.target_checkpoint_height(),
                    FinishedVerifying
                );
            }
            assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(1));
        }

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(1));

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn block_higher_than_max_checkpoint_fail() -> Result<(), Report> {
        zebra_test::init();

        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let block415000 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), block0.as_ref().into())]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for block 415000
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block415000.clone()),
        );
        /// Wait for the response for block 415000, and expect failure
        // TODO(teor || jlusby): check error kind
        let _ = verify_future
            .await
            .expect("timeout should not happen")
            .expect_err("bad block hash should fail");

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn wrong_checkpoint_hash_fail() -> Result<(), Report> {
        zebra_test::init();

        let good_block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let good_block0_hash: BlockHeaderHash = good_block0.as_ref().into();
        let mut bad_block0 =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        // Change the header hash
        bad_block0.header.version = 0;
        let bad_block0: Arc<Block> = bad_block0.into();

        // Make a checkpoint list containing the genesis block checkpoint
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(good_block0.coinbase_height().unwrap(), good_block0_hash)]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Make sure the verifier service is ready (1/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for bad block 0 (1/3)
        // TODO(teor || jlusby): check error kind
        let bad_verify_future_1 = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        // We can't await the future yet, because bad blocks aren't cleared
        // until the chain is verified

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Make sure the verifier service is ready (2/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for bad block 0 again (2/3)
        // TODO(teor || jlusby): check error kind
        let bad_verify_future_2 = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        // We can't await the future yet, because bad blocks aren't cleared
        // until the chain is verified

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Make sure the verifier service is ready (3/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for good block 0 (3/3)
        let good_verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(good_block0.clone()),
        );
        /// Wait for the response for good block 0, and expect success (3/3)
        // TODO(teor || jlusby): check error kind
        let verify_response = good_verify_future
            .await
            .expect("timeout should not happen")
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, good_block0_hash);

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        // Now, await the bad futures, which should have completed

        /// Wait for the response for block 0, and expect failure (1/3)
        // TODO(teor || jlusby): check error kind
        let _ = bad_verify_future_1
            .await
            .expect("timeout should not happen")
            .expect_err("bad block hash should fail");

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        /// Wait for the response for block 0, and expect failure again (2/3)
        // TODO(teor || jlusby): check error kind
        let _ = bad_verify_future_2
            .await
            .expect("timeout should not happen")
            .expect_err("bad block hash should fail");

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying
        );
        assert_eq!(checkpoint_verifier.max_checkpoint_height(), BlockHeight(0));

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_drop_cancel() -> Result<(), Report> {
        zebra_test::init();

        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((block.clone(), block.coinbase_height().unwrap(), hash));
        }

        // Make a checkpoint list containing all the blocks
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> = checkpoint_data
            .iter()
            .map(|(_block, height, hash)| (*height, *hash))
            .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            BeforeGenesis
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(
            checkpoint_verifier.max_checkpoint_height(),
            BlockHeight(434873)
        );

        let mut futures = Vec::new();
        // Now collect verify futures for each block
        for (block, height, hash) in checkpoint_data {
            /// Make sure the verifier service is ready
            let ready_verifier_service = checkpoint_verifier
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?;

            /// Set up the future for block {?height}
            let verify_future = timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                ready_verifier_service.call(block.clone()),
            );

            futures.push((verify_future, height, hash));

            // Only continuous checkpoints verify
            assert_eq!(
                checkpoint_verifier.previous_checkpoint_height(),
                PreviousCheckpoint(BlockHeight(min(height.0, 1)))
            );
            assert_eq!(
                checkpoint_verifier.target_checkpoint_height(),
                WaitingForBlocks
            );
            assert_eq!(
                checkpoint_verifier.max_checkpoint_height(),
                BlockHeight(434873)
            );
        }

        // Now drop the verifier, to cancel the futures
        drop(checkpoint_verifier);

        for (verify_future, height, hash) in futures {
            /// Check the response for block {?height}
            let verify_response = verify_future.await.expect("timeout should not happen");

            if height <= BlockHeight(1) {
                // The futures for continuous checkpoints should have succeeded before drop
                let verify_hash = verify_response.map_err(|e| eyre!(e))?;
                assert_eq!(verify_hash, hash);
            } else {
                // TODO(teor || jlusby): check error kind
                verify_response.expect_err("Pending futures should fail on drop");
            }
        }

        Ok(())
    }
}
