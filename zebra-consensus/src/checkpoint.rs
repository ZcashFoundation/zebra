//! Checkpoint-based block verification.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to
//! speed up the initial chain sync for Zebra. This list is distributed
//! with Zebra.
//!
//! The checkpoint verifier queues pending blocks. Once there is a
//! chain from the previous checkpoint to a target checkpoint, it
//! verifies all the blocks in that chain, and sends accepted blocks to
//! the state service as finalized chain state, skipping the majority of
//! contextual verification checks.
//!
//! Verification starts at the first checkpoint, which is the genesis
//! block for the configured network.

use std::{
    collections::BTreeMap,
    ops::{Bound, Bound::*},
    pin::Pin,
    sync::{mpsc, Arc},
    task::{Context, Poll},
};

use futures::{Future, FutureExt, TryFutureExt};
use thiserror::Error;
use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use tracing::instrument;

use zebra_chain::{
    amount,
    block::{self, Block},
    parameters::{
        checkpoint::list::CheckpointList, subsidy::SubsidyError, Network,
        GENESIS_PREVIOUS_BLOCK_HASH,
    },
    work::equihash,
};
use zebra_state::{self as zs, CheckpointVerifiedBlock};

use crate::{
    block::VerifyBlockError,
    checkpoint::types::{
        Progress::{self, *},
        TargetHeight::{self, *},
    },
    error::BlockError,
    BoxError,
};

mod types;

#[cfg(test)]
mod tests;

pub use zebra_node_services::constants::{MAX_CHECKPOINT_BYTE_COUNT, MAX_CHECKPOINT_HEIGHT_GAP};

/// An unverified block, which is in the queue for checkpoint verification.
#[derive(Debug)]
struct QueuedBlock {
    /// The block, with additional precalculated data.
    block: CheckpointVerifiedBlock,
    /// The transmitting end of the oneshot channel for this block's result.
    tx: oneshot::Sender<Result<block::Hash, VerifyCheckpointError>>,
}

/// The unverified block, with a receiver for the [`QueuedBlock`]'s result.
#[derive(Debug)]
struct RequestBlock {
    /// The block, with additional precalculated data.
    block: CheckpointVerifiedBlock,
    /// The receiving end of the oneshot channel for this block's result.
    rx: oneshot::Receiver<Result<block::Hash, VerifyCheckpointError>>,
}

/// A list of unverified blocks at a particular height.
///
/// Typically contains a single block, but might contain more if a peer
/// has an old chain fork. (Or sends us a bad block.)
///
/// The CheckpointVerifier avoids creating zero-block lists.
type QueuedBlockList = Vec<QueuedBlock>;

/// The maximum number of queued blocks at any one height.
///
/// This value is a tradeoff between:
/// - rejecting bad blocks: if we queue more blocks, we need fewer network
///   retries, but use a bit more CPU when verifying,
/// - avoiding a memory DoS: if we queue fewer blocks, we use less memory.
///
/// Memory usage is controlled by the sync service, because it controls block
/// downloads. When the verifier services process blocks, they reduce memory
/// usage by committing blocks to the disk state. (Or dropping invalid blocks.)
pub const MAX_QUEUED_BLOCKS_PER_HEIGHT: usize = 4;

/// Convert a tip into its hash and matching progress.
fn progress_from_tip(
    checkpoint_list: &CheckpointList,
    tip: Option<(block::Height, block::Hash)>,
) -> (Option<block::Hash>, Progress<block::Height>) {
    match tip {
        Some((height, hash)) => {
            if height >= checkpoint_list.max_height() {
                (None, Progress::FinalCheckpoint)
            } else {
                metrics::gauge!("checkpoint.verified.height").set(height.0 as f64);
                metrics::gauge!("checkpoint.processing.next.height").set(height.0 as f64);
                (Some(hash), Progress::InitialTip(height))
            }
        }
        // We start by verifying the genesis block, by itself
        None => (None, Progress::BeforeGenesis),
    }
}

/// A checkpointing block verifier.
///
/// Verifies blocks using a supplied list of checkpoints. There must be at
/// least one checkpoint for the genesis block.
pub struct CheckpointVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    /// The checkpoint list for this verifier.
    checkpoint_list: Arc<CheckpointList>,

    /// The network rules used by this verifier.
    network: Network,

    /// The hash of the initial tip, if any.
    initial_tip_hash: Option<block::Hash>,

    /// The underlying state service, possibly wrapped in other services.
    state_service: S,

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
    queued: BTreeMap<block::Height, QueuedBlockList>,

    /// The current progress of this verifier.
    verifier_progress: Progress<block::Height>,

    /// A channel to receive requests to reset the verifier,
    /// receiving the tip of the state.
    reset_receiver: mpsc::Receiver<Option<(block::Height, block::Hash)>>,
    /// A channel to send requests to reset the verifier,
    /// passing the tip of the state.
    reset_sender: mpsc::Sender<Option<(block::Height, block::Hash)>>,

    /// Queued block height progress transmitter.
    #[cfg(feature = "progress-bar")]
    queued_blocks_bar: howudoin::Tx,

    /// Verified checkpoint progress transmitter.
    #[cfg(feature = "progress-bar")]
    verified_checkpoint_bar: howudoin::Tx,
}

impl<S> std::fmt::Debug for CheckpointVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointVerifier")
            .field("checkpoint_list", &self.checkpoint_list)
            .field("network", &self.network)
            .field("initial_tip_hash", &self.initial_tip_hash)
            .field("queued", &self.queued)
            .field("verifier_progress", &self.verifier_progress)
            .finish()
    }
}

impl<S> CheckpointVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    /// Return a checkpoint verification service for `network`, using the
    /// hard-coded checkpoint list, and the provided `state_service`.
    ///
    /// If `initial_tip` is Some(_), the verifier starts at that initial tip.
    /// The initial tip can be between the checkpoints in the hard-coded
    /// checkpoint list.
    ///
    /// The checkpoint verifier holds a state service of type `S`, into which newly
    /// verified blocks will be committed. This state is pluggable to allow for
    /// testing or instrumentation.
    ///
    /// This function should be called only once for a particular network, rather
    /// than constructing multiple verification services for the same network. To
    /// clone a CheckpointVerifier, you might need to wrap it in a
    /// `tower::Buffer` service.
    #[allow(dead_code)]
    pub fn new(
        network: &Network,
        initial_tip: Option<(block::Height, block::Hash)>,
        state_service: S,
    ) -> Self {
        let checkpoint_list = network.checkpoint_list();
        let max_height = checkpoint_list.max_height();
        tracing::info!(
            ?max_height,
            ?network,
            ?initial_tip,
            "initialising CheckpointVerifier"
        );
        Self::from_checkpoint_list(checkpoint_list, network, initial_tip, state_service)
    }

    /// Return a checkpoint verification service using `list`, `network`,
    /// `initial_tip`, and `state_service`.
    ///
    /// Assumes that the provided genesis checkpoint is correct.
    ///
    /// Callers should prefer `CheckpointVerifier::new`, which uses the
    /// hard-coded checkpoint lists, or `CheckpointList::from_list` if you need
    /// to specify a custom checkpoint list. See those functions for more
    /// details.
    ///
    /// This function is designed for use in tests.
    #[allow(dead_code)]
    pub(crate) fn from_list(
        list: impl IntoIterator<Item = (block::Height, block::Hash)>,
        network: &Network,
        initial_tip: Option<(block::Height, block::Hash)>,
        state_service: S,
    ) -> Result<Self, VerifyCheckpointError> {
        Ok(Self::from_checkpoint_list(
            CheckpointList::from_list(list)
                .map(Arc::new)
                .map_err(VerifyCheckpointError::CheckpointList)?,
            network,
            initial_tip,
            state_service,
        ))
    }

    /// Return a checkpoint verification service using `checkpoint_list`,
    /// `network`, `initial_tip`, and `state_service`.
    ///
    /// Assumes that the provided genesis checkpoint is correct.
    ///
    /// Callers should prefer `CheckpointVerifier::new`, which uses the
    /// hard-coded checkpoint lists. See that function for more details.
    pub(crate) fn from_checkpoint_list(
        checkpoint_list: Arc<CheckpointList>,
        network: &Network,
        initial_tip: Option<(block::Height, block::Hash)>,
        state_service: S,
    ) -> Self {
        // All the initialisers should call this function, so we only have to
        // change fields or default values in one place.
        let (initial_tip_hash, verifier_progress) =
            progress_from_tip(&checkpoint_list, initial_tip);

        let (sender, receiver) = mpsc::channel();

        #[cfg(feature = "progress-bar")]
        let queued_blocks_bar = howudoin::new_root().label("Checkpoint Queue Height");

        #[cfg(feature = "progress-bar")]
        let verified_checkpoint_bar =
            howudoin::new_with_parent(queued_blocks_bar.id()).label("Verified Checkpoints");

        let verifier = CheckpointVerifier {
            checkpoint_list,
            network: network.clone(),
            initial_tip_hash,
            state_service,
            queued: BTreeMap::new(),
            verifier_progress,
            reset_receiver: receiver,
            reset_sender: sender,
            #[cfg(feature = "progress-bar")]
            queued_blocks_bar,
            #[cfg(feature = "progress-bar")]
            verified_checkpoint_bar,
        };

        if verifier_progress.is_final_checkpoint() {
            verifier.finish_diagnostics();
        } else {
            verifier.verified_checkpoint_diagnostics(verifier_progress.height());
        }

        verifier
    }

    /// Update diagnostics for queued blocks.
    fn queued_block_diagnostics(&self, height: block::Height, hash: block::Hash) {
        let max_queued_height = self
            .queued
            .keys()
            .next_back()
            .expect("queued has at least one entry");

        metrics::gauge!("checkpoint.queued.max.height").set(max_queued_height.0 as f64);

        let is_checkpoint = self.checkpoint_list.contains(height);
        tracing::debug!(?height, ?hash, ?is_checkpoint, "queued block");

        #[cfg(feature = "progress-bar")]
        if matches!(howudoin::cancelled(), Some(true)) {
            self.finish_diagnostics();
        } else {
            self.queued_blocks_bar
                .set_pos(max_queued_height.0)
                .set_len(u64::from(self.checkpoint_list.max_height().0));
        }
    }

    /// Update diagnostics for verified checkpoints.
    fn verified_checkpoint_diagnostics(&self, verified_height: impl Into<Option<block::Height>>) {
        let Some(verified_height) = verified_height.into() else {
            // We don't know if we have already finished, or haven't started yet,
            // so don't register any progress
            return;
        };

        metrics::gauge!("checkpoint.verified.height").set(verified_height.0 as f64);

        let checkpoint_index = self.checkpoint_list.prev_checkpoint_index(verified_height);
        let checkpoint_count = self.checkpoint_list.len();

        metrics::gauge!("checkpoint.verified.count").set(checkpoint_index as f64);

        tracing::debug!(
            ?verified_height,
            ?checkpoint_index,
            ?checkpoint_count,
            "verified checkpoint",
        );

        #[cfg(feature = "progress-bar")]
        if matches!(howudoin::cancelled(), Some(true)) {
            self.finish_diagnostics();
        } else {
            self.verified_checkpoint_bar
                .set_pos(u64::try_from(checkpoint_index).expect("fits in u64"))
                .set_len(u64::try_from(checkpoint_count).expect("fits in u64"));
        }
    }

    /// Finish checkpoint verifier diagnostics.
    fn finish_diagnostics(&self) {
        #[cfg(feature = "progress-bar")]
        {
            self.queued_blocks_bar.close();
            self.verified_checkpoint_bar.close();
        }
    }

    /// Reset the verifier progress back to given tip.
    fn reset_progress(&mut self, tip: Option<(block::Height, block::Hash)>) {
        let (initial_tip_hash, verifier_progress) = progress_from_tip(&self.checkpoint_list, tip);
        self.initial_tip_hash = initial_tip_hash;
        self.verifier_progress = verifier_progress;

        self.verified_checkpoint_diagnostics(verifier_progress.height());
    }

    /// Return the current verifier's progress.
    ///
    /// If verification has not started yet, returns `BeforeGenesis`,
    /// or `InitialTip(height)` if there were cached verified blocks.
    ///
    /// If verification is ongoing, returns `PreviousCheckpoint(height)`.
    /// `height` increases as checkpoints are verified.
    ///
    /// If verification has finished, returns `FinalCheckpoint`.
    fn previous_checkpoint_height(&self) -> Progress<block::Height> {
        self.verifier_progress
    }

    /// Return the start of the current checkpoint range.
    ///
    /// Returns None if verification has finished.
    fn current_start_bound(&self) -> Option<Bound<block::Height>> {
        match self.previous_checkpoint_height() {
            BeforeGenesis => Some(Unbounded),
            InitialTip(height) | PreviousCheckpoint(height) => Some(Excluded(height)),
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
    fn target_checkpoint_height(&self) -> TargetHeight {
        // Find the height we want to start searching at
        let start_height = match self.previous_checkpoint_height() {
            // Check if we have the genesis block as a special case, to simplify the loop
            BeforeGenesis if !self.queued.contains_key(&block::Height(0)) => {
                tracing::trace!("Waiting for genesis block");
                metrics::counter!("checkpoint.waiting.count").increment(1);
                return WaitingForBlocks;
            }
            BeforeGenesis => block::Height(0),
            InitialTip(height) | PreviousCheckpoint(height) => height,
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
        let mut pending_height = start_height;
        for (&height, _) in self.queued.range((Excluded(pending_height), Unbounded)) {
            // If the queued blocks are continuous.
            if height == block::Height(pending_height.0 + 1) {
                pending_height = height;
            } else {
                let gap = height.0 - pending_height.0;
                // Try to log a useful message when checkpointing has issues
                tracing::trace!(contiguous_height = ?pending_height,
                                next_height = ?height,
                                ?gap,
                                "Waiting for more checkpoint blocks");
                break;
            }
        }
        metrics::gauge!("checkpoint.queued.continuous.height").set(pending_height.0 as f64);

        // Now find the start of the checkpoint range
        let start = self.current_start_bound().expect(
            "if verification has finished, we should have returned earlier in the function",
        );
        // Find the highest checkpoint below pending_height, excluding any
        // previously verified checkpoints
        let target_checkpoint = self
            .checkpoint_list
            .max_height_in_range((start, Included(pending_height)));

        tracing::trace!(
            checkpoint_start = ?start,
            highest_contiguous_block = ?pending_height,
            ?target_checkpoint
        );

        if let Some(block::Height(target_checkpoint)) = target_checkpoint {
            metrics::gauge!("checkpoint.processing.next.height").set(target_checkpoint as f64);
        } else {
            // Use the start height if there is no potential next checkpoint
            metrics::gauge!("checkpoint.processing.next.height").set(start_height.0 as f64);
            metrics::counter!("checkpoint.waiting.count").increment(1);
        }

        target_checkpoint
            .map(Checkpoint)
            .unwrap_or(WaitingForBlocks)
    }

    /// Return the most recently verified checkpoint's hash.
    ///
    /// See `previous_checkpoint_height()` for details.
    fn previous_checkpoint_hash(&self) -> Progress<block::Hash> {
        match self.previous_checkpoint_height() {
            BeforeGenesis => BeforeGenesis,
            InitialTip(_) => self
                .initial_tip_hash
                .map(InitialTip)
                .expect("initial tip height must have an initial tip hash"),
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
    fn check_height(&self, height: block::Height) -> Result<(), VerifyCheckpointError> {
        if height > self.checkpoint_list.max_height() {
            Err(VerifyCheckpointError::TooHigh {
                height,
                max_height: self.checkpoint_list.max_height(),
            })?;
        }

        match self.previous_checkpoint_height() {
            // Any height is valid
            BeforeGenesis => {}
            // Greater heights are valid
            InitialTip(previous_height) | PreviousCheckpoint(previous_height)
                if (height <= previous_height) =>
            {
                let e = Err(VerifyCheckpointError::AlreadyVerified {
                    height,
                    verified_height: previous_height,
                });
                tracing::trace!(?e);
                e?;
            }
            InitialTip(_) | PreviousCheckpoint(_) => {}
            // We're finished, so no checkpoint height is valid
            FinalCheckpoint => Err(VerifyCheckpointError::Finished)?,
        };

        Ok(())
    }

    /// Increase the current checkpoint height to `verified_height`,
    fn update_progress(&mut self, verified_height: block::Height) {
        if let Some(max_height) = self.queued.keys().next_back() {
            metrics::gauge!("checkpoint.queued.max.height").set(max_height.0 as f64);
        } else {
            // use f64::NAN as a sentinel value for "None", because 0 is a valid height
            metrics::gauge!("checkpoint.queued.max.height").set(f64::NAN);
        }
        metrics::gauge!("checkpoint.queued_slots").set(self.queued.len() as f64);

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

            tracing::info!(
                final_checkpoint_height = ?verified_height,
                "verified final checkpoint: starting full validation",
            );

            self.verified_checkpoint_diagnostics(verified_height);
            self.finish_diagnostics();
        } else if self.checkpoint_list.contains(verified_height) {
            self.verifier_progress = PreviousCheckpoint(verified_height);
            // We're done with the initial tip hash now
            self.initial_tip_hash = None;

            self.verified_checkpoint_diagnostics(verified_height);
        }
    }

    /// Check that the block height, proof of work, and Merkle root are valid.
    ///
    /// Returns a [`CheckpointVerifiedBlock`] with precalculated block data.
    ///
    /// ## Security
    ///
    /// Checking the proof of work makes resource exhaustion attacks harder to
    /// carry out, because malicious blocks require a valid proof of work.
    ///
    /// Checking the Merkle root ensures that the block hash binds the block
    /// contents. To prevent malleability (CVE-2012-2459), we also need to check
    /// whether the transaction hashes are unique.
    fn check_block(
        &self,
        block: Arc<Block>,
    ) -> Result<CheckpointVerifiedBlock, VerifyCheckpointError> {
        let hash = block.hash();
        let height = block
            .coinbase_height()
            .ok_or(VerifyCheckpointError::CoinbaseHeight { hash })?;
        self.check_height(height)?;

        if self.network.disable_pow() {
            crate::block::check::difficulty_threshold_is_valid(
                &block.header,
                &self.network,
                &height,
                &hash,
            )?;
        } else {
            crate::block::check::difficulty_is_valid(&block.header, &self.network, &height, &hash)?;
            crate::block::check::equihash_solution_is_valid(&block.header)?;
        }

        // don't do precalculation until the block passes basic difficulty checks
        let block = CheckpointVerifiedBlock::new(block, Some(hash), None);

        crate::block::check::merkle_root_validity(
            &self.network,
            &block.block,
            &block.transaction_hashes,
        )?;

        Ok(block)
    }

    /// Queue `block` for verification.
    ///
    /// On success, returns a [`RequestBlock`] containing the block,
    /// precalculated request data, and the queued result receiver.
    ///
    /// Verification will finish when the chain to the next checkpoint is
    /// complete, and the caller will be notified via the channel.
    ///
    /// If the block does not pass basic validity checks,
    /// returns an error immediately.
    #[allow(clippy::unwrap_in_result)]
    fn queue_block(&mut self, block: Arc<Block>) -> Result<RequestBlock, VerifyCheckpointError> {
        // Set up a oneshot channel to send results
        let (tx, rx) = oneshot::channel();

        // Check that the height and Merkle roots are valid.
        let block = self.check_block(block)?;
        let height = block.height;
        let hash = block.hash;

        let new_qblock = QueuedBlock {
            block: block.clone(),
            tx,
        };
        let req_block = RequestBlock { block, rx };

        // Since we're using Arc<Block>, each entry is a single pointer to the
        // Arc. But there are a lot of QueuedBlockLists in the queue, so we keep
        // allocations as small as possible.
        let qblocks = self
            .queued
            .entry(height)
            .or_insert_with(|| QueuedBlockList::with_capacity(1));

        // Replace older requests with newer ones.
        // The newer block is ok, the older block is an error.
        for qb in qblocks.iter_mut() {
            if qb.block.hash == hash {
                let e = VerifyCheckpointError::NewerRequest { height, hash };
                tracing::trace!(?e, "failing older of duplicate requests");

                // ## Security
                //
                // Replace the entire queued block.
                //
                // We don't check the authorizing data hash until checkpoint blocks reach the state.
                // So signatures, proofs, or scripts could be different,
                // even if the block hash is the same.

                let old = std::mem::replace(qb, new_qblock);
                let _ = old.tx.send(Err(e));
                return Ok(req_block);
            }
        }

        // Memory DoS resistance: limit the queued blocks at each height
        if qblocks.len() >= MAX_QUEUED_BLOCKS_PER_HEIGHT {
            let e = VerifyCheckpointError::QueuedLimit;
            tracing::warn!(?e);
            return Err(e);
        }

        // Add the block to the list of queued blocks at this height
        // This is a no-op for the first block in each QueuedBlockList.
        qblocks.reserve_exact(1);
        qblocks.push(new_qblock);

        self.queued_block_diagnostics(height, hash);

        Ok(req_block)
    }

    /// During checkpoint range processing, process all the blocks at `height`.
    ///
    /// Returns the first valid block. If there is no valid block, returns None.
    #[allow(clippy::unwrap_in_result)]
    fn process_height(
        &mut self,
        height: block::Height,
        expected_hash: block::Hash,
    ) -> Option<QueuedBlock> {
        let mut qblocks = self
            .queued
            .remove(&height)
            .expect("the current checkpoint range has continuous Vec<QueuedBlock>s");
        assert!(
            !qblocks.is_empty(),
            "the current checkpoint range has continuous Blocks"
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
        //   - one of the blocks matches the chain (the common case)
        //   - no blocks match the chain, verification has failed for this range
        // If there are any side-chain blocks, they fail validation.
        let mut valid_qblock = None;
        for qblock in qblocks.drain(..) {
            if qblock.block.hash == expected_hash {
                if valid_qblock.is_none() {
                    // The first valid block at the current height
                    valid_qblock = Some(qblock);
                } else {
                    unreachable!("unexpected duplicate block {:?} {:?}: duplicate blocks should be rejected before being queued",
                                 height, qblock.block.hash);
                }
            } else {
                tracing::info!(?height, ?qblock.block.hash, ?expected_hash,
                               "Side chain hash at height in CheckpointVerifier");
                let _ = qblock
                    .tx
                    .send(Err(VerifyCheckpointError::UnexpectedSideChain {
                        found: qblock.block.hash,
                        expected: expected_hash,
                    }));
            }
        }

        valid_qblock
    }

    /// Try to verify from the previous checkpoint to a target checkpoint.
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
            BeforeGenesis => GENESIS_PREVIOUS_BLOCK_HASH,
            InitialTip(hash) | PreviousCheckpoint(hash) => hash,
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
            WaitingForBlocks => {
                return;
            }
            FinishedVerifying => {
                unreachable!("the FinalCheckpoint case should have returned earlier")
            }
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
        let range_heights: Vec<block::Height> = self
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
                expected_hash = qblock.block.block.header.previous_block_hash;
                // Add the block to the end of the pending block list
                // (since we're walking the chain backwards, the list is
                // in reverse chain order)
                rev_valid_blocks.push(qblock);
            } else {
                // The last block height we processed did not have any blocks
                // with a matching hash, so chain verification has failed.
                tracing::info!(
                    ?current_height,
                    ?current_range,
                    "No valid blocks at height in CheckpointVerifier"
                );

                // We kept all the matching blocks down to this height, in
                // anticipation of the chain verifying. But the chain is
                // incomplete, so we have to put them back in the queue.
                //
                // The order here shouldn't matter, but add the blocks in
                // height order, for consistency.
                for vblock in rev_valid_blocks.drain(..).rev() {
                    self.queued
                        .entry(vblock.block.height)
                        .or_default()
                        .push(vblock);
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

        let block_count = rev_valid_blocks.len();
        tracing::info!(?block_count, ?current_range, "verified checkpoint range");
        metrics::counter!("checkpoint.verified.block.count").increment(block_count as u64);

        // All the blocks we've kept are valid, so let's verify them
        // in height order.
        for qblock in rev_valid_blocks.drain(..).rev() {
            // Sending can fail, but there's nothing we can do about it.
            let _ = qblock.tx.send(Ok(qblock.block.hash));
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
impl<S> Drop for CheckpointVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    /// Send an error on `tx` for any `QueuedBlock`s that haven't been verified.
    ///
    /// We can't implement `Drop` on QueuedBlock, because `send()` consumes
    /// `tx`. And `tx` doesn't implement `Copy` or `Default` (for `take()`).
    fn drop(&mut self) {
        self.finish_diagnostics();

        let drop_keys: Vec<_> = self.queued.keys().cloned().collect();
        for key in drop_keys {
            let mut qblocks = self
                .queued
                .remove(&key)
                .expect("each entry is only removed once");
            for qblock in qblocks.drain(..) {
                // Sending can fail, but there's nothing we can do about it.
                let _ = qblock.tx.send(Err(VerifyCheckpointError::Dropped));
            }
        }
    }
}

#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum VerifyCheckpointError {
    #[error("checkpoint request after the final checkpoint has been verified")]
    Finished,
    #[error("block at {height:?} is higher than the maximum checkpoint {max_height:?}")]
    TooHigh {
        height: block::Height,
        max_height: block::Height,
    },
    #[error("block {height:?} is less than or equal to the verified tip {verified_height:?}")]
    AlreadyVerified {
        height: block::Height,
        verified_height: block::Height,
    },
    #[error("rejected older of duplicate verification requests for block at {height:?} {hash:?}")]
    NewerRequest {
        height: block::Height,
        hash: block::Hash,
    },
    #[error("the block {hash:?} does not have a coinbase height")]
    CoinbaseHeight { hash: block::Hash },
    #[error("merkle root {actual:?} does not match expected {expected:?}")]
    BadMerkleRoot {
        actual: block::merkle::Root,
        expected: block::merkle::Root,
    },
    #[error("duplicate transactions in block")]
    DuplicateTransaction,
    #[error("checkpoint verifier was dropped")]
    Dropped,
    #[error(transparent)]
    CommitCheckpointVerified(BoxError),
    #[error(transparent)]
    Tip(BoxError),
    #[error(transparent)]
    CheckpointList(BoxError),
    #[error(transparent)]
    VerifyBlock(VerifyBlockError),
    #[error("invalid block subsidy: {0}")]
    SubsidyError(#[from] SubsidyError),
    #[error("invalid amount: {0}")]
    AmountError(#[from] amount::Error),
    #[error("too many queued blocks at this height")]
    QueuedLimit,
    #[error("the block hash does not match the chained checkpoint hash, expected {expected:?} found {found:?}")]
    UnexpectedSideChain {
        expected: block::Hash,
        found: block::Hash,
    },
    #[error("zebra is shutting down")]
    ShuttingDown,
}

impl From<VerifyBlockError> for VerifyCheckpointError {
    fn from(err: VerifyBlockError) -> VerifyCheckpointError {
        VerifyCheckpointError::VerifyBlock(err)
    }
}

impl From<BlockError> for VerifyCheckpointError {
    fn from(err: BlockError) -> VerifyCheckpointError {
        VerifyCheckpointError::VerifyBlock(err.into())
    }
}

impl From<equihash::Error> for VerifyCheckpointError {
    fn from(err: equihash::Error) -> VerifyCheckpointError {
        VerifyCheckpointError::VerifyBlock(err.into())
    }
}

impl VerifyCheckpointError {
    /// Returns `true` if this is definitely a duplicate request.
    /// Some duplicate requests might not be detected, and therefore return `false`.
    pub fn is_duplicate_request(&self) -> bool {
        match self {
            VerifyCheckpointError::AlreadyVerified { .. } => true,
            // TODO: make this duplicate-incomplete
            VerifyCheckpointError::NewerRequest { .. } => true,
            VerifyCheckpointError::VerifyBlock(block_error) => block_error.is_duplicate_request(),
            _ => false,
        }
    }

    /// Returns a suggested misbehaviour score increment for a certain error.
    pub fn misbehavior_score(&self) -> u32 {
        // TODO: Adjust these values based on zcashd (#9258).
        match self {
            VerifyCheckpointError::VerifyBlock(verify_block_error) => {
                verify_block_error.misbehavior_score()
            }
            VerifyCheckpointError::SubsidyError(_)
            | VerifyCheckpointError::CoinbaseHeight { .. }
            | VerifyCheckpointError::DuplicateTransaction
            | VerifyCheckpointError::AmountError(_) => 100,
            _other => 0,
        }
    }
}

/// The CheckpointVerifier service implementation.
///
/// After verification, the block futures resolve to their hashes.
impl<S> Service<Arc<Block>> for CheckpointVerifier<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = block::Hash;
    type Error = VerifyCheckpointError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[instrument(name = "checkpoint", skip(self, block))]
    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // Reset the verifier back to the state tip if requested
        // (e.g. due to an error when committing a block to the state)
        if let Ok(tip) = self.reset_receiver.try_recv() {
            self.reset_progress(tip);
        }

        // Immediately reject all incoming blocks that arrive after we've finished.
        if let FinalCheckpoint = self.previous_checkpoint_height() {
            return async { Err(VerifyCheckpointError::Finished) }.boxed();
        }

        let req_block = match self.queue_block(block) {
            Ok(req_block) => req_block,
            Err(e) => return async { Err(e) }.boxed(),
        };

        self.process_checkpoint_range();

        metrics::gauge!("checkpoint.queued_slots").set(self.queued.len() as f64);

        // Because the checkpoint verifier duplicates state from the state
        // service (it tracks which checkpoints have been verified), we must
        // commit blocks transactionally on a per-checkpoint basis. Otherwise,
        // the checkpoint verifier's state could desync from the underlying
        // state service. Among other problems, this could cause the checkpoint
        // verifier to reject blocks not already in the state as
        // already-verified.
        //
        // # Dropped Receivers
        //
        // To commit blocks transactionally on a per-checkpoint basis, we must
        // commit all verified blocks in a checkpoint range, regardless of
        // whether or not the response futures for each block were dropped.
        //
        // We accomplish this by spawning a new task containing the
        // commit-if-verified logic. This task will always execute, except if
        // the program is interrupted, in which case there is no longer a
        // checkpoint verifier to keep in sync with the state.
        //
        // # State Commit Failures
        //
        // If the state commit fails due to corrupt block data,
        // we don't reject the entire checkpoint.
        // Instead, we reset the verifier to the successfully committed state tip.
        let state_service = self.state_service.clone();
        let commit_checkpoint_verified = tokio::spawn(async move {
            let hash = req_block
                .rx
                .await
                .map_err(Into::into)
                .map_err(VerifyCheckpointError::CommitCheckpointVerified)
                .expect("CheckpointVerifier does not leave dangling receivers")?;

            // We use a `ServiceExt::oneshot`, so that every state service
            // `poll_ready` has a corresponding `call`. See #1593.
            match state_service
                .oneshot(zs::Request::CommitCheckpointVerifiedBlock(req_block.block))
                .map_err(VerifyCheckpointError::CommitCheckpointVerified)
                .await?
            {
                zs::Response::Committed(committed_hash) => {
                    assert_eq!(committed_hash, hash, "state must commit correct hash");
                    Ok(hash)
                }
                _ => unreachable!("wrong response for CommitCheckpointVerifiedBlock"),
            }
        });

        let state_service = self.state_service.clone();
        let reset_sender = self.reset_sender.clone();
        async move {
            let result = commit_checkpoint_verified.await;
            // Avoid a panic on shutdown
            //
            // When `zebrad` is terminated using Ctrl-C, the `commit_checkpoint_verified` task
            // can return a `JoinError::Cancelled`. We expect task cancellation on shutdown,
            // so we don't need to panic here. The persistent state is correct even when the
            // task is cancelled, because block data is committed inside transactions, in
            // height order.
            let result = if zebra_chain::shutdown::is_shutting_down() {
                Err(VerifyCheckpointError::ShuttingDown)
            } else {
                result.expect("commit_checkpoint_verified should not panic")
            };
            if result.is_err() {
                // If there was an error committing the block, then this verifier
                // will be out of sync with the state. In that case, reset
                // its progress back to the state tip.
                let tip = match state_service
                    .oneshot(zs::Request::Tip)
                    .await
                    .map_err(VerifyCheckpointError::Tip)?
                {
                    zs::Response::Tip(tip) => tip,
                    _ => unreachable!("wrong response for Tip"),
                };
                // Ignore errors since send() can fail only when the verifier
                // is being dropped, and then it doesn't matter anymore.
                let _ = reset_sender.send(tip);
            }
            result
        }
        .boxed()
    }
}
