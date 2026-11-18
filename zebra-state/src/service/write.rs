//! Writing blocks to the finalized and non-finalized states.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use indexmap::IndexMap;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot, watch,
};

use tracing::Span;
use zebra_chain::{
    block::{self, Height},
    parallel::tree::NoteCommitmentTrees,
};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{
        check,
        fast_tree_sync::{CheckpointCommit, FastTreeSyncWriter, TreeRootsCache},
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
        ChainTipBlock, ChainTipSender, InvalidateError, PreciousError, ReconsiderError,
    },
    CheckpointVerifiedBlock, SemanticallyVerifiedBlock, ValidateContextError,
};

// These types are used in doc links
#[allow(unused_imports)]
use crate::service::{
    chain_tip::{ChainTipChange, LatestChainTip},
    non_finalized_state::Chain,
};

/// The maximum size of the parent error map.
///
/// We allow enough space for multiple concurrent chain forks with errors.
const PARENT_ERROR_MAP_LIMIT: usize = MAX_BLOCK_REORG_HEIGHT as usize * 2;

/// Bounds one exact-body recovery session, including malicious replacement replays.
#[cfg(not(test))]
const BODY_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
#[cfg(test)]
const BODY_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Run contextual validation on the prepared block and add it to the
/// non-finalized state if it is contextually valid.
#[tracing::instrument(
    level = "debug",
    skip(finalized_state, non_finalized_state, prepared),
    fields(
        height = ?prepared.height,
        hash = %prepared.hash,
        chains = non_finalized_state.chain_count()
    )
)]
pub(crate) fn validate_and_commit_non_finalized(
    finalized_state: &ZebraDb,
    non_finalized_state: &mut NonFinalizedState,
    prepared: SemanticallyVerifiedBlock,
) -> Result<(), ValidateContextError> {
    check::initial_contextual_validity(finalized_state, non_finalized_state, &prepared)?;
    let parent_hash = prepared.block.header.previous_block_hash;

    if finalized_state.finalized_tip_hash() == parent_hash {
        non_finalized_state.commit_new_chain(prepared, finalized_state)?;
    } else {
        non_finalized_state.commit_block(prepared, finalized_state)?;
    }

    Ok(())
}

/// Finalizes the best chain's root blocks while [`NonFinalizedState::should_finalize_root`],
/// waking the pruning worker after each durable commit.
fn finalize_root_blocks(
    finalized_state: &mut FinalizedState,
    non_finalized_state: &mut NonFinalizedState,
    prev_finalized_note_commitment_trees: &mut Option<NoteCommitmentTrees>,
    pruning_wake: Option<&crossbeam_channel::Sender<()>>,
) {
    while non_finalized_state.should_finalize_root() {
        tracing::trace!("finalizing a block past the reorg or index entry limit");
        let contextually_verified_with_trees = non_finalized_state.finalize();
        *prev_finalized_note_commitment_trees = finalized_state
            .commit_finalized_direct(
                contextually_verified_with_trees,
                prev_finalized_note_commitment_trees.take(),
                "commit contextually-verified request",
            )
            .expect(
                "unexpected finalized block commit error: note commitment and history trees were \
                 already checked by the non-finalized state",
            )
            .1
            .into();
        if let Some(wake) = pruning_wake {
            let _ = wake.try_send(());
        }
    }
}

/// What the block write task did with a non-finalized block, so the state service can update the
/// blocks and outputs it tracks as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NonFinalizedWriteOutcome {
    /// The block was rejected, or evicted from every chain, so it isn't in the state.
    Rejected(block::Hash),

    /// The block was committed, and the state published to readers contains it.
    Committed(block::Hash),
}

/// Sends the hashes of blocks evicted from the non-finalized state on `non_finalized_rejected_sender`,
/// so the state service accepts them again if the network extends their chain.
///
/// Returns `true` if any blocks were evicted.
fn forget_evicted_blocks(
    non_finalized_state: &mut NonFinalizedState,
    non_finalized_rejected_sender: &UnboundedSender<NonFinalizedWriteOutcome>,
) -> bool {
    let evicted = non_finalized_state.take_evicted_block_hashes();
    for &hash in &evicted {
        // If the receiver was dropped, the state service is shutting down.
        let _ = non_finalized_rejected_sender.send(NonFinalizedWriteOutcome::Rejected(hash));
    }

    !evicted.is_empty()
}

/// Finalizes root blocks after a commit, then reports any forks that finalizing evicted, because
/// [`NonFinalizedState::finalize`] re-inserts the remaining chains under the fork limits, and
/// publishes the state if it changed.
#[allow(clippy::too_many_arguments)]
fn finalize_root_blocks_and_forget_evicted(
    finalized_state: &mut FinalizedState,
    non_finalized_state: &mut NonFinalizedState,
    prev_finalized_note_commitment_trees: &mut Option<NoteCommitmentTrees>,
    non_finalized_rejected_sender: &UnboundedSender<NonFinalizedWriteOutcome>,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
    pruning_wake: Option<&crossbeam_channel::Sender<()>>,
) {
    let root_height = non_finalized_state.root_height();
    finalize_root_blocks(
        finalized_state,
        non_finalized_state,
        prev_finalized_note_commitment_trees,
        pruning_wake,
    );

    // Keep the old snapshot's roots visible until the database contains them.
    if forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender)
        || non_finalized_state.root_height() != root_height
    {
        update_latest_chain_channels(
            non_finalized_state,
            &finalized_state.db,
            chain_tip_sender,
            non_finalized_state_sender,
            backup_dir_path,
        );
    }
}

/// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
/// channels with the latest state. If there is no non-finalized chain, use the
/// finalized tip.
///
/// If `backup_dir_path` is `Some`, the non-finalized state is written to the backup
/// directory before updating the channels.
///
/// Returns the latest chain tip height, or `None` if both states are empty, and the snapshot
/// that the new one replaced. Callers drop it after they respond, because freeing a large
/// snapshot would otherwise delay the tip update and the response.
#[instrument(
    level = "debug",
    skip(
        non_finalized_state,
        finalized_state,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path,
    ),
    fields(chains = non_finalized_state.chain_count())
)]
fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    finalized_state: &ZebraDb,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
) -> (Option<block::Height>, NonFinalizedState) {
    let non_finalized_tip = non_finalized_state
        .best_tip_block()
        .cloned()
        .map(ChainTipBlock::from);
    let tip_block_height = if let Some(tip) = &non_finalized_tip {
        Some(tip.height)
    } else {
        finalized_state.finalized_tip_height()
    };

    if let Some(backup_dir_path) = backup_dir_path {
        non_finalized_state.write_to_backup(backup_dir_path);
    }

    let replaced_snapshot = non_finalized_state_sender.send_replace(non_finalized_state.clone());

    if let Some(tip) = non_finalized_tip {
        chain_tip_sender.set_best_non_finalized_tip(tip);
    } else {
        let finalized_tip = finalized_state
            .tip_block()
            .expect("the finalized tip must have a complete stored body")
            .map(crate::CheckpointVerifiedBlock::from)
            .map(ChainTipBlock::from);
        chain_tip_sender.clear_best_non_finalized_tip(finalized_tip);
    }

    (tip_block_height, replaced_snapshot)
}

/// A worker task that reads, validates, and writes blocks to the
/// `finalized_state` or `non_finalized_state`.
struct WriteBlockWorkerTask {
    finalized_block_write_receiver: UnboundedReceiver<QueuedCheckpointVerified>,
    non_finalized_block_write_receiver: UnboundedReceiver<NonFinalizedWriteMessage>,
    finalized_state: FinalizedState,
    non_finalized_state: NonFinalizedState,
    /// Reports queue resets or requests an exact replacement for a malformed finalized body.
    invalid_block_reset_sender: UnboundedSender<FinalizedWriteFeedback>,
    /// Signals the [`crate::service::StateService`] that a non-finalized block was rejected by
    /// the write task, so its hash should be removed from
    /// `non_finalized_block_write_sent_hashes`.
    ///
    /// Without this, a rejected same-hash block locks out a later honest
    /// re-delivery of a block at the same hash as a "duplicate" until restart
    /// or reorg.
    non_finalized_rejected_sender: UnboundedSender<NonFinalizedWriteOutcome>,
    chain_tip_sender: ChainTipSender,
    non_finalized_state_sender: watch::Sender<NonFinalizedState>,
    /// If `Some`, the non-finalized state is written to this backup directory
    /// synchronously before each channel update, instead of via the async backup task.
    backup_dir_path: Option<PathBuf>,
    /// Peer-supplied note commitment tree roots for fast checkpoint sync.
    tree_roots_cache: Option<TreeRootsCache>,
    /// Coalesces durable finalized-tip notifications for the pruning worker.
    pruning_wake: Option<crossbeam_channel::Sender<()>>,
}

/// Finalized writer recovery, either a full commit failure or one replaceable body.
#[derive(Debug)]
pub(super) enum FinalizedWriteFeedback {
    /// Reset to the durable finalized tip after a failed current-block commit.
    Reset(Option<block::Height>, block::Hash),
    /// Replace only this body; descendants remain queued in their existing channel.
    InvalidBody {
        /// The checkpoint-authenticated hash whose authorizing data was malformed.
        hash: block::Hash,
        /// One-slot reply, used once by the state without changing last-sent ordering.
        replacement: std::sync::mpsc::SyncSender<QueuedCheckpointVerified>,
    },
}

/// The message type for the non-finalized block write task channel.
pub enum NonFinalizedWriteMessage {
    /// A newly downloaded and semantically verified block prepared for
    /// contextual validation and insertion into the non-finalized state,
    /// with the time it was sent to the writer.
    Commit(QueuedSemanticallyVerified, Instant),
    /// The hash of a block that should be invalidated and removed from
    /// the non-finalized state, if present.
    Invalidate {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<block::Hash, InvalidateError>>,
    },
    /// The hash of a block that was previously invalidated but should be
    /// reconsidered and reinserted into the non-finalized state.
    Reconsider {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<Vec<block::Hash>, ReconsiderError>>,
    },
    // Portions copyright (c) 2026 Zakura Contributors, licensed under the MIT license or the
    // Apache License, Version 2.0, at your option.
    // Adapted from zakura-core/zakura#1117 (https://github.com/zakura-core/zakura/pull/1117).
    /// The hash of a block to prefer over equal-work chain tips.
    Precious {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<block::Hash, PreciousError>>,
    },
}

/// A worker with a task that reads, validates, and writes blocks to the
/// `finalized_state` or `non_finalized_state` and channels for sending
/// it blocks.
#[derive(Clone, Debug)]
pub(super) struct BlockWriteSender {
    /// A channel to send blocks to the `block_write_task`,
    /// so they can be written to the [`NonFinalizedState`].
    pub non_finalized: Option<tokio::sync::mpsc::UnboundedSender<NonFinalizedWriteMessage>>,

    /// A channel to send blocks to the `block_write_task`,
    /// so they can be written to the [`FinalizedState`].
    ///
    /// This sender is dropped after the state has finished sending all the checkpointed blocks,
    /// and the lowest semantically verified block arrives.
    pub finalized: Option<tokio::sync::mpsc::UnboundedSender<QueuedCheckpointVerified>>,
}

impl BlockWriteSender {
    /// Creates a new [`BlockWriteSender`] with the given receivers and states.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        level = "debug",
        skip_all,
        fields(
            network = %non_finalized_state.network
        )
    )]
    pub fn spawn(
        finalized_state: FinalizedState,
        non_finalized_state: NonFinalizedState,
        chain_tip_sender: ChainTipSender,
        non_finalized_state_sender: watch::Sender<NonFinalizedState>,
        should_use_finalized_block_write_sender: bool,
        backup_dir_path: Option<PathBuf>,
        tree_roots_cache: TreeRootsCache,
        pruning_wake: Option<crossbeam_channel::Sender<()>>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<FinalizedWriteFeedback>,
        tokio::sync::mpsc::UnboundedReceiver<NonFinalizedWriteOutcome>,
        Option<Arc<std::thread::JoinHandle<()>>>,
    ) {
        // Security: The number of blocks in these channels is limited by
        //           the syncer and inbound lookahead limits.
        let (non_finalized_block_write_sender, non_finalized_block_write_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (finalized_block_write_sender, finalized_block_write_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (invalid_block_reset_sender, invalid_block_write_reset_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (non_finalized_rejected_sender, non_finalized_rejected_receiver) =
            tokio::sync::mpsc::unbounded_channel();

        let span = Span::current();
        let task = std::thread::spawn(move || {
            // The writer's parallel work, such as note commitment tree updates, gets its own
            // pool: on the global pool, a worker waiting in a `join` can steal a long unrelated
            // job, like decoding a whole block, and hold up the block being committed.
            // The writer loop occupies one worker, leaving the global pool's thread count for work.
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(rayon::current_num_threads() + 1)
                .thread_name(|index| format!("state writer {index}"))
                .start_handler(|_| {
                    rocksdb::perf::set_perf_stats(rocksdb::perf::PerfStatsLevel::Disable);
                })
                .build()
                .expect("startup thread creation only fails when the OS is out of resources");

            pool.install(|| {
                span.in_scope(|| {
                    WriteBlockWorkerTask {
                        finalized_block_write_receiver,
                        non_finalized_block_write_receiver,
                        finalized_state,
                        non_finalized_state,
                        invalid_block_reset_sender,
                        non_finalized_rejected_sender,
                        chain_tip_sender,
                        non_finalized_state_sender,
                        backup_dir_path,
                        tree_roots_cache: Some(tree_roots_cache),
                        pruning_wake,
                    }
                    .run()
                })
            })
        });

        (
            Self {
                non_finalized: Some(non_finalized_block_write_sender),
                finalized: should_use_finalized_block_write_sender
                    .then_some(finalized_block_write_sender),
            },
            invalid_block_write_reset_receiver,
            non_finalized_rejected_receiver,
            Some(Arc::new(task)),
        )
    }
}

/// What happened to a checkpoint-verified block the writer tried to commit.
enum CommitOutcome {
    /// The block is committed.
    Committed(Option<ValidateContextError>),
    /// The block failed, and the state service was told to reset its queue.
    Failed,
    /// The state service is shutting down.
    Shutdown,
}

/// Commits a checkpoint-verified block to the finalized state, and reports the result to the
/// state service and the block's sender, waking the pruning worker after a durable commit.
///
/// If `next` is the block after it, fast checkpoint sync can commit the block with its verified
/// tree roots, without updating its note commitment trees.
#[allow(clippy::too_many_arguments)]
fn commit_checkpoint_verified(
    mut ordered_block: QueuedCheckpointVerified,
    next: Option<&CheckpointVerifiedBlock>,
    finalized_state: &mut FinalizedState,
    prev_finalized_note_commitment_trees: &mut Option<NoteCommitmentTrees>,
    fast_tree_sync: &mut FastTreeSyncWriter,
    chain_tip_sender: &mut ChainTipSender,
    invalid_block_reset_sender: &UnboundedSender<FinalizedWriteFeedback>,
    pruning_wake: Option<&crossbeam_channel::Sender<()>>,
) -> CommitOutcome {
    // Roots prove effects, not a V5 body's authorizing data. Recover a malformed current body
    // before consuming it, including the first fast block; keep the final commit-boundary check.
    if next.is_some() {
        if let Err(error) = check::block_commitment_is_valid_for_chain_history(
            ordered_block.0.block.clone(),
            &finalized_state.network(),
            &finalized_state.db.history_tree(),
            ordered_block.0.precomputed_auth_data_root(),
        ) {
            if error.is_auth_commitment_mismatch() {
                ordered_block = match replace_checkpoint_body(
                    ordered_block,
                    error,
                    finalized_state,
                    invalid_block_reset_sender,
                ) {
                    Ok(replacement) => replacement,
                    Err(outcome) => return outcome,
                };
            }
        }
    }
    let committed_permit = match ordered_block
        .2
        .as_ref()
        .map(crate::MinedBlockNotifications::try_reserve_committed)
        .transpose()
    {
        Ok(permit) => permit.flatten(),
        Err(error) => {
            // Publish the reset before replying so an immediate retry can make progress.
            let _ = invalid_block_reset_sender.send(FinalizedWriteFeedback::Reset(
                finalized_state.db.finalized_tip_height(),
                finalized_state.db.finalized_tip_hash(),
            ));
            let _ = ordered_block.1.send(Err(error.into()));
            return CommitOutcome::Failed;
        }
    };

    // Try committing the block, but keep notifications until its context is published.
    let (checkpoint_verified, rsp_tx, _) = ordered_block;
    let height = checkpoint_verified.height;
    let commit = match next {
        Some(next) => {
            let trees = prev_finalized_note_commitment_trees
                .get_or_insert_with(|| finalized_state.db.note_commitment_trees_for_tip());
            fast_tree_sync.checkpoint_commit(
                checkpoint_verified,
                next,
                &finalized_state.db.history_tree(),
                trees,
                || invalid_block_reset_sender.is_closed(),
            )
        }
        None => CheckpointCommit::Normal(checkpoint_verified),
    };

    let (result, frozen_sizes, successor_error) = match commit {
        CheckpointCommit::Fast {
            block,
            frozen_sizes,
            successor_error,
        } => (
            finalized_state
                .commit_finalized_fast(*block, prev_finalized_note_commitment_trees.take()),
            frozen_sizes,
            successor_error,
        ),
        CheckpointCommit::Normal(block) => {
            assert!(
                !fast_tree_sync.is_frozen(),
                "blocks are never committed with tree updates from frozen trees",
            );
            (
                finalized_state
                    .commit_finalized(block, prev_finalized_note_commitment_trees.take()),
                None,
                None,
            )
        }
        CheckpointCommit::Stalled(height) => panic!(
            "fast checkpoint sync stalled at {height:?}: no authenticated tree roots for 120 seconds. \
             Connect to peers advertising NODE_TREE_ROOTS and restart. Turning off \
             state.fast_tree_sync cannot thaw this database; alternatively restore a backup from \
             before fast sync, or delete this network's state cache under state.cache_dir and \
             resync with state.fast_tree_sync = false."
        ),
        CheckpointCommit::Shutdown => return CommitOutcome::Shutdown,
    };

    let result = match result {
        Ok((tip_block, note_commitment_trees)) => {
            let hash = tip_block.hash;
            *prev_finalized_note_commitment_trees = Some(note_commitment_trees);
            fast_tree_sync.committed(height, frozen_sizes);
            chain_tip_sender.set_finalized_tip(tip_block);
            if let Some(wake) = pruning_wake {
                let _ = wake.try_send(());
            }
            Ok(hash)
        }
        Err(error) => {
            let finalized_tip = finalized_state.db.tip();

            // The last block in the queue failed, so we can't commit the next block.
            // Instead, we need to reset the state queue,
            // and discard any children of the invalid block in the channel.
            info!(
                ?error,
                last_valid_height = ?finalized_tip.map(|tip| tip.0),
                last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                "committing a block to the finalized state failed, resetting state queue",
            );

            let send_result = invalid_block_reset_sender.send(FinalizedWriteFeedback::Reset(
                finalized_state.db.finalized_tip_height(),
                finalized_state.db.finalized_tip_hash(),
            ));

            if send_result.is_err() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                let _ = rsp_tx.send(Err(error));
                return CommitOutcome::Shutdown;
            }
            Err(error)
        }
    };

    let outcome = if result.is_ok() {
        CommitOutcome::Committed(successor_error)
    } else {
        CommitOutcome::Failed
    };
    if let (Ok(hash), Some(permit)) = (&result, committed_permit) {
        permit.send((*hash, height));
    }
    let _ = rsp_tx.send(result);

    outcome
}

/// Rejects a malformed successor on its own completion, preserving queued descendants.
///
/// Only the exact checkpoint hash can use the one-slot reply. Waiting does not drain or reset
/// the ordinary block channel. Expiry resets the queue without stopping the writer.
fn replace_checkpoint_body(
    mut queued: QueuedCheckpointVerified,
    mut error: ValidateContextError,
    finalized_state: &FinalizedState,
    feedback: &UnboundedSender<FinalizedWriteFeedback>,
) -> Result<QueuedCheckpointVerified, CommitOutcome> {
    let hash = queued.0.hash;
    let deadline = Instant::now() + BODY_RECOVERY_TIMEOUT;
    loop {
        assert!(
            error.is_auth_commitment_mismatch(),
            "only authorizing data is replaceable"
        );
        tracing::warn!(malformed_body_hash = ?hash, %error, "rejected checkpoint authorizing data");
        let (replacement, receiver) = std::sync::mpsc::sync_channel(1);
        let recovery_requested = feedback
            .send(FinalizedWriteFeedback::InvalidBody { hash, replacement })
            .is_ok();
        let (block, response, notifications) = queued;
        drop((block, notifications));
        let reported_error = if recovery_requested {
            crate::CommitCheckpointVerifiedError::targeted_body_recovery(error)
        } else {
            error.into()
        };
        let _ = response.send(Err(reported_error));
        if !recovery_requested {
            return Err(CommitOutcome::Shutdown);
        }
        queued = loop {
            if feedback.is_closed() {
                return Err(CommitOutcome::Shutdown);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    ?hash,
                    "checkpoint body recovery expired, resetting state queue"
                );
                if feedback
                    .send(FinalizedWriteFeedback::Reset(
                        finalized_state.db.finalized_tip_height(),
                        finalized_state.db.finalized_tip_hash(),
                    ))
                    .is_err()
                {
                    return Err(CommitOutcome::Shutdown);
                }
                return Err(CommitOutcome::Failed);
            }
            match receiver.recv_timeout(remaining.min(std::time::Duration::from_secs(1))) {
                Ok(queued) => break queued,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CommitOutcome::Shutdown);
                }
            }
        };
        assert_eq!(
            queued.0.hash, hash,
            "the state routes only the exact requested replacement"
        );
        match check::block_commitment_is_valid_for_chain_history(
            queued.0.block.clone(),
            &finalized_state.network(),
            &finalized_state.db.history_tree(),
            queued.0.precomputed_auth_data_root(),
        ) {
            Ok(()) => return Ok(queued),
            Err(next_error) => error = next_error,
        }
    }
}

impl WriteBlockWorkerTask {
    /// Reads blocks from the channels, writes them to the `finalized_state` or `non_finalized_state`,
    /// sends any errors on the `invalid_block_reset_sender`, then updates the `chain_tip_sender` and
    /// `non_finalized_state_sender`.
    #[instrument(
        level = "debug",
        skip(self),
        fields(
            network = %self.non_finalized_state.network
        )
    )]
    pub fn run(mut self) {
        let Self {
            finalized_block_write_receiver,
            non_finalized_block_write_receiver,
            finalized_state,
            non_finalized_state,
            invalid_block_reset_sender,
            non_finalized_rejected_sender,
            chain_tip_sender,
            non_finalized_state_sender,
            backup_dir_path,
            tree_roots_cache,
            pruning_wake,
        } = &mut self;

        let mut prev_finalized_note_commitment_trees = None;
        let mut fast_tree_sync = FastTreeSyncWriter::new(
            &finalized_state.db,
            tree_roots_cache
                .take()
                .expect("only the writer takes ownership of tree roots"),
            finalized_state.network(),
        );
        // Fast checkpoint sync holds a block until the next block, which verifies its tree roots.
        let mut held_block: Option<QueuedCheckpointVerified> = None;

        // Write all the finalized blocks sent by the state,
        // until the state closes the finalized block channel's sender.
        while let Some(mut ordered_block) = finalized_block_write_receiver.blocking_recv() {
            // TODO: split these checks into separate functions

            if invalid_block_reset_sender.is_closed() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                return;
            }

            // Discard any children of invalid blocks in the channel
            //
            // `commit_finalized()` requires blocks in height order.
            // So if there has been a block commit error,
            // we need to drop all the descendants of that block,
            // until we receive a block at the required next height.
            let next_valid_height = match &held_block {
                Some(held_block) => {
                    (held_block.0.height + 1).expect("held heights are below the maximum height")
                }
                None => finalized_state
                    .db
                    .finalized_tip_height()
                    .map(|height| (height + 1).expect("committed heights are valid"))
                    .unwrap_or(Height(0)),
            };

            if ordered_block.0.height != next_valid_height {
                debug!(
                    ?next_valid_height,
                    invalid_height = ?ordered_block.0.height,
                    invalid_hash = ?ordered_block.0.hash,
                    "got a block that was the wrong height. \
                     Assuming a parent block failed, and dropping this block",
                );

                // We don't want to send a reset here, because it could overwrite a valid sent hash
                std::mem::drop(ordered_block);
                continue;
            }

            if let Some(held_block) = held_block.take() {
                match commit_checkpoint_verified(
                    held_block,
                    Some(&ordered_block.0),
                    finalized_state,
                    &mut prev_finalized_note_commitment_trees,
                    &mut fast_tree_sync,
                    chain_tip_sender,
                    invalid_block_reset_sender,
                    pruning_wake.as_ref(),
                ) {
                    CommitOutcome::Committed(Some(error)) => {
                        ordered_block = match replace_checkpoint_body(
                            ordered_block,
                            error,
                            finalized_state,
                            invalid_block_reset_sender,
                        ) {
                            Ok(replacement) => replacement,
                            Err(CommitOutcome::Failed) => continue,
                            Err(_) => return,
                        };
                    }
                    CommitOutcome::Committed(None) => {}
                    // Drop this child of the failed block.
                    CommitOutcome::Failed => continue,
                    CommitOutcome::Shutdown => return,
                }
            }

            if fast_tree_sync.defers(&ordered_block.0) {
                held_block = Some(ordered_block);
                continue;
            }

            if let CommitOutcome::Shutdown = commit_checkpoint_verified(
                ordered_block,
                None,
                finalized_state,
                &mut prev_finalized_note_commitment_trees,
                &mut fast_tree_sync,
                chain_tip_sender,
                invalid_block_reset_sender,
                pruning_wake.as_ref(),
            ) {
                return;
            }
        }

        // Do this check even if the channel got closed before any finalized blocks were sent.
        // This can happen if we're past the finalized tip.
        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // A restored backup can hold more blocks than the limits allow, so finalize them before
        // committing any new blocks.
        let restored_root = non_finalized_state.root_height();
        finalize_root_blocks(
            finalized_state,
            non_finalized_state,
            &mut prev_finalized_note_commitment_trees,
            pruning_wake.as_ref(),
        );
        if non_finalized_state.root_height() != restored_root {
            forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);
            update_latest_chain_channels(
                non_finalized_state,
                &finalized_state.db,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            );
        }

        // Save any errors to propagate down to queued child blocks
        let mut parent_error_map: IndexMap<block::Hash, ValidateContextError> = IndexMap::new();

        while let Some(msg) = non_finalized_block_write_receiver.blocking_recv() {
            let queued_child_and_rsp_tx = match msg {
                NonFinalizedWriteMessage::Commit(queued_child, queued_at) => {
                    Some((queued_child, queued_at))
                }
                NonFinalizedWriteMessage::Invalidate { hash, rsp_tx } => {
                    tracing::info!(?hash, "invalidating a block in the non-finalized state");
                    let _ = rsp_tx.send(non_finalized_state.invalidate_block(hash));
                    None
                }
                NonFinalizedWriteMessage::Reconsider { hash, rsp_tx } => {
                    tracing::info!(?hash, "reconsidering a block in the non-finalized state");
                    let _ = rsp_tx
                        .send(non_finalized_state.reconsider_block(hash, &finalized_state.db));
                    None
                }
                // Adapted from zakura-core/zakura#1117
                // (https://github.com/zakura-core/zakura/pull/1117).
                NonFinalizedWriteMessage::Precious { hash, rsp_tx } => {
                    tracing::info!(?hash, "preferring a block in the non-finalized state");
                    let result = non_finalized_state.precious_block(hash, &finalized_state.db);
                    update_latest_chain_channels(
                        non_finalized_state,
                        &finalized_state.db,
                        chain_tip_sender,
                        non_finalized_state_sender,
                        backup_dir_path.as_deref(),
                    );
                    let _ = rsp_tx.send(result);
                    continue;
                }
            };

            let Some(((queued_child, rsp_tx, contextual, notifications), queued_at)) =
                queued_child_and_rsp_tx
            else {
                forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);
                update_latest_chain_channels(
                    non_finalized_state,
                    &finalized_state.db,
                    chain_tip_sender,
                    non_finalized_state_sender,
                    backup_dir_path.as_deref(),
                );
                continue;
            };

            metrics::histogram!("state.block_writer.phase.duration_seconds", "phase" => "queue")
                .record(queued_at.elapsed().as_secs_f64());

            let child_hash = queued_child.hash;
            let child_height = queued_child.height;
            let parent_hash = queued_child.block.header.previous_block_hash;
            let parent_error = parent_error_map.get(&parent_hash);

            // Never wait for relay capacity or reserve it while a block is queued for its parent.
            // Admission failure is operational, not evidence that this block or its children are invalid.
            let committed_permit = if parent_error.is_none() {
                match notifications
                    .as_ref()
                    .map(crate::MinedBlockNotifications::try_reserve_committed)
                    .transpose()
                {
                    Ok(permit) => permit.flatten(),
                    Err(error) => {
                        let _ = non_finalized_rejected_sender
                            .send(NonFinalizedWriteOutcome::Rejected(child_hash));
                        let _ = rsp_tx.send(Err(error.into()));
                        continue;
                    }
                }
            } else {
                None
            };

            // If the parent block was marked as rejected, also reject all its children.
            //
            // At this point, we know that all the block's descendants
            // are invalid, because we checked all the consensus rules before
            // committing the failing ancestor block to the non-finalized state.
            //
            // The children are served by other peers, which often queue them before
            // the parent arrives, so they get a distinct error that doesn't carry the
            // parent's misbehaviour score.
            let result = if let Some(parent_error) = parent_error {
                Err(parent_error.for_descendant(parent_hash))
            } else {
                // Measure only commits here, not proposals or restored backup blocks.
                let phase_start = Instant::now();
                let result = if let Some(mut contextual) = contextual {
                    // A replaced identical body keeps its first receipt time.
                    contextual.received_time = queued_child.received_time;
                    non_finalized_state.commit_prevalidated_contextual(
                        *contextual,
                        &finalized_state.db,
                        notifications.as_ref(),
                    )
                } else {
                    tracing::trace!(?child_hash, "validating queued child");
                    validate_and_commit_non_finalized(
                        &finalized_state.db,
                        non_finalized_state,
                        queued_child,
                    )
                };
                metrics::histogram!(
                    "state.block_writer.phase.duration_seconds",
                    "phase" => "contextual_commit"
                )
                .record(phase_start.elapsed().as_secs_f64());
                result
            };

            if let Err(ref error) = result {
                // If the block is invalid, mark any descendant blocks as rejected.
                parent_error_map.insert(child_hash, error.clone());

                // Make sure the error map doesn't get too big.
                if parent_error_map.len() > PARENT_ERROR_MAP_LIMIT {
                    // We only add one hash at a time, so we only need to remove one extra here.
                    parent_error_map.shift_remove_index(0);
                }

                // Release relay capacity before either notification can wake an immediate replay.
                drop(committed_permit);

                // Signal the StateService to drop this hash from
                // `non_finalized_block_write_sent_hashes`, so a subsequent
                // re-delivery of a block at the same hash is not short-circuited
                // as a "duplicate" against a rejected variant that never reached
                // any chain.
                //
                // If the receiver was dropped (the StateService is shutting
                // down), ignore the error: the lockout cannot matter once the
                // service exits.
                let _ = non_finalized_rejected_sender
                    .send(NonFinalizedWriteOutcome::Rejected(child_hash));

                // Update the caller with the error.
                let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

                // Skip the things we only need to do for successfully committed blocks
                continue;
            }

            // A successfully committed block supersedes any contextual error
            // recorded for a different block body with the same header hash.
            parent_error_map.shift_remove(&child_hash);

            forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);

            let phase_start = Instant::now();
            let (tip_block_height, replaced_snapshot) = update_latest_chain_channels(
                non_finalized_state,
                &finalized_state.db,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            );
            let tip_block_height =
                tip_block_height.expect("just successfully inserted a non-finalized block above");
            metrics::histogram!(
                "state.block_writer.phase.duration_seconds",
                "phase" => "update_latest_chain_channels"
            )
            .record(phase_start.elapsed().as_secs_f64());

            // Readers can now find the block's outputs in the published state, so the state
            // service doesn't need its own copies.
            let _ =
                non_finalized_rejected_sender.send(NonFinalizedWriteOutcome::Committed(child_hash));

            // Ownership stays in this writer even if the caller dropped its response future.
            // Notify side-chain commits too, using the committed block rather than the best tip.
            if let Some(permit) = committed_permit {
                permit.send((child_hash, child_height));
            }

            // Acknowledge published state, not completion of older blocks' retention writes.
            let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));
            drop(replaced_snapshot);

            // Finish retention before receiving another block; finalization errors remain fatal.
            let phase_start = Instant::now();
            finalize_root_blocks_and_forget_evicted(
                finalized_state,
                non_finalized_state,
                &mut prev_finalized_note_commitment_trees,
                non_finalized_rejected_sender,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
                pruning_wake.as_ref(),
            );
            metrics::histogram!(
                "state.block_writer.phase.duration_seconds",
                "phase" => "finalization"
            )
            .record(phase_start.elapsed().as_secs_f64());

            // Update the metrics if semantic and contextual validation passes
            //
            // TODO: split this out into a function?
            metrics::counter!("state.full_verifier.committed.block.count").increment(1);
            metrics::counter!("zcash.chain.verified.block.total").increment(1);

            metrics::gauge!("state.full_verifier.committed.block.height")
                .set(tip_block_height.0 as f64);

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because this block write task makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height").set(tip_block_height.0 as f64);

            tracing::trace!("finished processing queued block");
        }

        // We're finished receiving non-finalized blocks from the state, and
        // done writing to the finalized state, so we can force it to shut down.
        finalized_state.db.shutdown(true);
        std::mem::drop(self.finalized_state);
    }
}

#[cfg(test)]
mod tests;
