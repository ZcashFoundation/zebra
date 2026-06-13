//! Writing blocks to the finalized and non-finalized states.

mod finalized_write_phase;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
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
    request::FinalizableBlock,
    service::{
        check,
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
        write::finalized_write_phase::FinalizedWritePhase,
        ChainTipBlock, ChainTipSender, InvalidateError, ReconsiderError,
    },
    SemanticallyVerifiedBlock, ValidateContextError,
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

/// The minimum value for the checkpoint-sync retention and pipeline
/// configs: half the rollback window.
///
/// See
/// [`Config::checkpoint_sync_retained_blocks`](crate::Config::checkpoint_sync_retained_blocks)
/// and
/// [`Config::checkpoint_sync_pipeline_capacity`](crate::Config::checkpoint_sync_pipeline_capacity);
/// configured values below this are raised to it.
const MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS: u32 = MAX_BLOCK_REORG_HEIGHT / 2;

/// The disk-writer tip height value meaning nothing has been written to disk
/// yet: `u32::MAX`, far above any real block height.
const NO_DISK_TIP_HEIGHT: u32 = u32::MAX;

/// Prunes blocks that the disk writer has already written from the
/// non-finalized state, keeping its memory use bounded by the pipeline
/// capacity.
///
/// `disk_writer_tip_height` is the height most recently published by the disk
/// writer, or [`NO_DISK_TIP_HEIGHT`] if nothing has been written yet.
///
/// While the non-finalized state's recently-finalized cache is enabled, each
/// pruned root leaves its still-spendable outputs behind in memory, so
/// checkpoint-sync spend lookups avoid database point reads (see
/// [`PrunedChain`](crate::service::non_finalized_state::PrunedChain)).
///
/// # Correctness
///
/// The `Acquire` load pairs with the disk writer's `Release` store, which
/// happens **after** `commit_finalized_direct` returns: reading height `H`
/// here therefore happens-after the completion of `H`'s database write, so a
/// block is never pruned from the in-memory non-finalized state before its
/// on-disk copy is in place — readers always find the block in one of the
/// two. `Relaxed` would not document or guarantee that prune-after-write
/// ordering (it would lean on RocksDB's internal synchronization instead);
/// the explicit `Release`/`Acquire` pair makes the invariant independent of
/// database internals.
fn prune_finalized_blocks(
    non_finalized_state: &mut NonFinalizedState,
    disk_writer_tip_height: &AtomicU32,
) {
    let disk_tip = disk_writer_tip_height.load(Ordering::Acquire);
    if disk_tip == NO_DISK_TIP_HEIGHT {
        return;
    }

    let finalized_tip_height = Height(disk_tip);
    while non_finalized_state
        .root_height()
        .is_some_and(|root_height| root_height <= finalized_tip_height)
    {
        non_finalized_state.finalize();
    }
}

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

/// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
/// channels with the latest non-finalized [`ChainTipBlock`] and
/// [`Chain`].
///
/// `last_zebra_mined_log_height` is used to rate-limit logging.
///
/// If `backup_dir_path` is `Some`, the non-finalized state is written to the backup
/// directory before updating the channels.
///
/// Returns the latest non-finalized chain tip height.
///
/// # Panics
///
/// If the `non_finalized_state` is empty.
#[instrument(
    level = "debug",
    skip(
        non_finalized_state,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path,
    ),
    fields(chains = non_finalized_state.chain_count())
)]
fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
) -> block::Height {
    let best_chain = non_finalized_state.best_chain().expect("unexpected empty non-finalized state: must commit at least one block before updating channels");

    let tip_block = best_chain
        .tip_block()
        .expect("unexpected empty chain: must commit at least one block before updating channels")
        .clone();
    let tip_block = ChainTipBlock::from(tip_block);

    let tip_block_height = tip_block.height;

    if let Some(backup_dir_path) = backup_dir_path {
        non_finalized_state.write_to_backup(backup_dir_path);
    }

    // If the final receiver was just dropped, ignore the error.
    let _ = non_finalized_state_sender.send(non_finalized_state.clone());

    chain_tip_sender.set_best_non_finalized_tip(tip_block);

    tip_block_height
}

/// A worker task that reads, validates, and writes blocks to the
/// `finalized_state` or `non_finalized_state`.
struct WriteBlockWorkerTask {
    finalized_block_write_receiver: UnboundedReceiver<QueuedCheckpointVerified>,
    non_finalized_block_write_receiver: UnboundedReceiver<NonFinalizedWriteMessage>,
    finalized_state: FinalizedState,
    non_finalized_state: NonFinalizedState,
    invalid_block_reset_sender: UnboundedSender<block::Hash>,
    /// Signals the [`crate::service::StateService`] that a non-finalized block was rejected by
    /// the write task, so its hash should be removed from
    /// `non_finalized_block_write_sent_hashes`.
    ///
    /// Without this, a rejected same-hash block locks out a later honest
    /// re-delivery of a block at the same hash as a "duplicate" until restart
    /// or reorg.
    non_finalized_rejected_sender: UnboundedSender<block::Hash>,
    chain_tip_sender: ChainTipSender,
    non_finalized_state_sender: watch::Sender<NonFinalizedState>,
    /// If `Some`, the non-finalized state is written to this backup directory
    /// synchronously before each channel update, instead of via the async backup task.
    backup_dir_path: Option<PathBuf>,
}

/// The message type for the non-finalized block write task channel.
pub enum NonFinalizedWriteMessage {
    /// A newly downloaded and semantically verified block prepared for
    /// contextual validation and insertion into the non-finalized state.
    Commit(QueuedSemanticallyVerified),
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
}

impl From<QueuedSemanticallyVerified> for NonFinalizedWriteMessage {
    fn from(block: QueuedSemanticallyVerified) -> Self {
        NonFinalizedWriteMessage::Commit(block)
    }
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
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<block::Hash>,
        tokio::sync::mpsc::UnboundedReceiver<block::Hash>,
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
                }
                .run()
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
        } = &mut self;

        let mut prev_finalized_note_commitment_trees = None;

        // Checkpoint pipeline: commit checkpoint-verified blocks to the
        // non-finalized state (Thread 1, this thread), then prepare the
        // database batch and write it to disk (Thread 2). Thread 1 responds
        // to the state service as soon as the block is in the non-finalized
        // state, so block commits are not serialized behind disk I/O; the
        // bounded channel limits how many blocks have disk writes in flight.
        //
        // Thread 1 does NOT publish the non-finalized state to the watch
        // channel while the pipeline runs: keeping the chain `Arc` uniquely
        // owned lets each commit mutate it in place instead of deep-cloning
        // it, and keeps the backup task idle (as it is on the non-pipelined
        // path). In-flight blocks are not queryable through the read service
        // until their disk writes complete, at most a pipeline depth later.
        //
        // The height of the block most recently written to disk by Thread 2,
        // published so Thread 1 can prune the non-finalized state without
        // re-reading the finalized tip from the database on every block. A
        // reverse iterator over the height column family gets slower as level 0
        // grows during the compaction-paused write phase, so re-reading it per
        // block would scale with sync depth.
        //
        // Correctness: Thread 2 `Release`-stores a height only after its
        // database write completes, and Thread 1 `Acquire`-loads it before
        // pruning, so a block is never pruned from the in-memory state before
        // its on-disk copy is in place (see `prune_finalized_blocks`). The
        // value is monotonic because only Thread 2 stores to it after the
        // genesis commit, in commit order.
        let disk_writer_tip_height = Arc::new(AtomicU32::new(
            finalized_state
                .db
                .finalized_tip_height()
                .map_or(NO_DISK_TIP_HEIGHT, |height| height.0),
        ));

        // Cache the spendable outputs of recently finalized blocks while the
        // pipeline runs, so checkpoint-sync spend lookups stay in memory
        // (see `PrunedChain`). Configured values below the minimum are
        // raised to it.
        let retained_blocks = {
            let configured = finalized_state.db.config().checkpoint_sync_retained_blocks;
            if configured < MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS {
                warn!(
                    configured,
                    minimum = MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS,
                    "checkpoint_sync_retained_blocks is below the minimum, \
                     using the minimum instead",
                );
            }
            configured.max(MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS)
        };
        non_finalized_state.enable_pruned_chain(retained_blocks);

        // How far the disk writes may lag the in-memory commits. Configured
        // values below the minimum are raised to it.
        let pipeline_capacity = {
            let configured = finalized_state
                .db
                .config()
                .checkpoint_sync_pipeline_capacity;
            let minimum = MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS as usize;
            if configured < minimum {
                warn!(
                    configured,
                    minimum,
                    "checkpoint_sync_pipeline_capacity is below the minimum, \
                     using the minimum instead",
                );
            }
            configured.max(minimum)
        };

        // Returns true if the state service is shutting down.
        let shutting_down = std::thread::scope(|s| {
            let (write_tx, write_rx) =
                crossbeam_channel::bounded::<FinalizableBlock>(pipeline_capacity);
            let disk_writer_tip_height_writer = disk_writer_tip_height.clone();

            // Thread 2: commit each block to the finalized state.
            //
            // Going through `commit_finalized_direct` keeps the tip linkage
            // assertions, the `debug_stop_at_height` exit, and elasticsearch
            // indexing that the sequential loop had.
            let mut write_state = finalized_state.clone();
            s.spawn(move || {
                // Pause auto-compaction while the finalized (checkpoint)
                // write phase is active, so background compactions don't
                // compete with the bulk block writes for disk bandwidth. If
                // the user opted in via `disable_wal_during_ibd`, also skip
                // the write-ahead log during this phase.
                //
                // This guard restores both settings on every exit path: it
                // drops when the pipeline drains, which the thread scope
                // waits for on every return from Thread 1, including panic
                // unwinds. If the process is killed without unwinding
                // (e.g. `kill -9`), the next database open re-enables
                // auto-compaction (see `DiskDb::set_auto_compaction`).
                let mut finalized_write_phase = FinalizedWritePhase::new(
                    write_state.db.clone(),
                    write_state.db.config().disable_wal_during_ibd,
                );

                let mut prev_note_commitment_trees: Option<NoteCommitmentTrees> = None;
                while let Ok(finalizable) = write_rx.recv() {
                    let height = match &finalizable {
                        FinalizableBlock::Contextual {
                            contextually_verified,
                            ..
                        } => contextually_verified.height,
                        FinalizableBlock::Checkpoint {
                            checkpoint_verified,
                        } => checkpoint_verified.height,
                    };

                    let (_hash, note_commitment_trees) = write_state
                        .commit_finalized_direct(
                            finalizable,
                            prev_note_commitment_trees.take(),
                            "checkpoint pipeline disk writer",
                        )
                        .expect(
                            "unexpected disk write error: the block was already \
                             committed to the non-finalized state",
                        );

                    prev_note_commitment_trees = Some(note_commitment_trees);

                    // Publish the on-disk tip height for Thread 1's prune loop.
                    // Release pairs with Thread 1's Acquire load so it never
                    // prunes a block before its disk write is visible.
                    disk_writer_tip_height_writer.store(height.0, Ordering::Release);

                    // Bound level 0 file growth while auto-compaction is paused.
                    finalized_write_phase.block_committed();

                    metrics::counter!("state.checkpoint.finalized.block.count").increment(1);
                    metrics::gauge!("state.checkpoint.finalized.block.height").set(height.0 as f64);
                    metrics::gauge!("zcash.chain.verified.block.height").set(height.0 as f64);
                    metrics::counter!("zcash.chain.verified.block.total").increment(1);
                }
            });

            // Thread 1: commit all the checkpoint-verified blocks sent by the
            // state to the non-finalized state, in height order, and feed
            // them into the pipeline; until the state closes the finalized
            // block channel's sender.
            //
            // Blocks already written to disk by the disk writer can't be on
            // disk at the wrong height, so Thread 1 tracks the next expected
            // height itself instead of re-reading the finalized tip.
            let mut next_expected_height = finalized_state
                .db
                .finalized_tip_height()
                .map(|height| (height + 1).expect("committed heights are valid"))
                .unwrap_or(Height(0));

            while let Some((checkpoint_verified, rsp_tx)) =
                finalized_block_write_receiver.blocking_recv()
            {
                if invalid_block_reset_sender.is_closed() {
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    return true;
                }

                // Discard any children of invalid blocks in the channel.
                //
                // The pipeline requires blocks in height order. So if there
                // has been a block commit error, we need to drop all the
                // descendants of that block, until we receive a block at the
                // required next height.
                if checkpoint_verified.height != next_expected_height {
                    debug!(
                        ?next_expected_height,
                        invalid_height = ?checkpoint_verified.height,
                        invalid_hash = ?checkpoint_verified.hash,
                        "got a block that was the wrong height. \
                         Assuming a parent block failed, and dropping this block",
                    );

                    // We don't want to send a reset here, because it could overwrite a valid sent hash
                    std::mem::drop((checkpoint_verified, rsp_tx));
                    continue;
                }

                // The genesis block must be committed directly to disk: the
                // non-finalized state's chains initialize their tree states
                // from a finalized tip, which doesn't exist yet.
                if checkpoint_verified.height == Height(0) {
                    // Genesis is always the first commit, so there are no
                    // previous note commitment trees to pass through.
                    match finalized_state.commit_finalized((checkpoint_verified, rsp_tx), None) {
                        Ok((finalized, _note_commitment_trees)) => {
                            let tip_block = ChainTipBlock::from(finalized);
                            chain_tip_sender.set_finalized_tip(tip_block);

                            // Genesis is committed to disk on this thread, so
                            // publish its height directly rather than via the
                            // disk writer.
                            disk_writer_tip_height.store(Height(0).0, Ordering::Release);

                            next_expected_height =
                                (next_expected_height + 1).expect("committed heights are valid");
                        }
                        Err(error) => {
                            let finalized_tip = finalized_state.db.tip();

                            info!(
                                ?error,
                                last_valid_height = ?finalized_tip.map(|tip| tip.0),
                                last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                                "committing the genesis block to the finalized state failed, \
                                 resetting state queue",
                            );

                            let send_result = invalid_block_reset_sender
                                .send(finalized_state.db.finalized_tip_hash());

                            if send_result.is_err() {
                                info!(
                                    "StateService closed the block reset channel. \
                                     Is Zebra shutting down?"
                                );
                                return true;
                            }
                        }
                    }

                    continue;
                }

                let hash = checkpoint_verified.hash;

                // Commit the block to the non-finalized state: a fast,
                // in-memory commit that updates the trees, nullifiers, and
                // UTXOs without the disk write. It also returns the finalizable
                // block, with its treestate already computed during the chain
                // update and its spent UTXOs already resolved by the commit.
                let (tip_block, finalizable) = non_finalized_state
                    .commit_checkpoint_block(checkpoint_verified, &finalized_state.db)
                    .expect(
                        "checkpoint block commits to the non-finalized state can't fail: \
                         the checkpoint hash chain pins the block's contents, and the \
                         database the chain context is read from is consistent",
                    );

                chain_tip_sender.set_best_non_finalized_tip(tip_block);

                if write_tx.send(finalizable).is_err() {
                    // The disk writer panicked; exiting unwinds the scope.
                    return true;
                }

                // Respond to the state service now: the block is committed
                // in memory, and the disk write can only fail by panicking.
                let _ = rsp_tx.send(Ok(hash));

                // The disk writer publishes its tip height, so this doesn't
                // re-read the finalized tip from the database on every block.
                prune_finalized_blocks(non_finalized_state, &disk_writer_tip_height);

                next_expected_height =
                    (next_expected_height + 1).expect("committed heights are valid");
            }

            // Dropping the write sender drains and joins the disk writer
            // when the scope exits; its disk writes complete before the
            // loops below run.
            drop(write_tx);
            false
        });

        if shutting_down {
            return;
        }

        // All the pipelined blocks are on disk: remove them from the
        // non-finalized state, so the full-verification phase below starts
        // from an empty non-finalized state at the finalized tip.
        //
        // Only blocks at or below the finalized tip are removed: if the
        // non-finalized state was restored from a backup at startup (so the
        // pipeline never ran), its blocks are above the finalized tip, and
        // they must be preserved for the full-verification phase. The thread
        // scope joined the disk writer, so its published tip height is final.
        prune_finalized_blocks(non_finalized_state, &disk_writer_tip_height);

        // The bulk-write phase is over: drop the recently-finalized cache
        // before the full-verification phase.
        non_finalized_state.disable_pruned_chain();

        // Do this check even if the channel got closed before any finalized blocks were sent.
        // This can happen if we're past the finalized tip.
        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // Save any errors to propagate down to queued child blocks
        let mut parent_error_map: IndexMap<block::Hash, ValidateContextError> = IndexMap::new();

        while let Some(msg) = non_finalized_block_write_receiver.blocking_recv() {
            let queued_child_and_rsp_tx = match msg {
                NonFinalizedWriteMessage::Commit(queued_child) => Some(queued_child),
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
            };

            let Some((queued_child, rsp_tx)) = queued_child_and_rsp_tx else {
                update_latest_chain_channels(
                    non_finalized_state,
                    chain_tip_sender,
                    non_finalized_state_sender,
                    backup_dir_path.as_deref(),
                );
                continue;
            };

            let child_hash = queued_child.hash;
            let parent_hash = queued_child.block.header.previous_block_hash;
            let parent_error = parent_error_map.get(&parent_hash);

            // If the parent block was marked as rejected, also reject all its children.
            //
            // At this point, we know that all the block's descendants
            // are invalid, because we checked all the consensus rules before
            // committing the failing ancestor block to the non-finalized state.
            let result = if let Some(parent_error) = parent_error {
                Err(parent_error.clone())
            } else {
                tracing::trace!(?child_hash, "validating queued child");
                validate_and_commit_non_finalized(
                    &finalized_state.db,
                    non_finalized_state,
                    queued_child,
                )
            };

            // TODO: fix the test timing bugs that require the result to be sent
            //       after `update_latest_chain_channels()`,
            //       and send the result on rsp_tx here

            if let Err(ref error) = result {
                // If the block is invalid, mark any descendant blocks as rejected.
                parent_error_map.insert(child_hash, error.clone());

                // Make sure the error map doesn't get too big.
                if parent_error_map.len() > PARENT_ERROR_MAP_LIMIT {
                    // We only add one hash at a time, so we only need to remove one extra here.
                    parent_error_map.shift_remove_index(0);
                }

                // Signal the StateService to drop this hash from
                // `non_finalized_block_write_sent_hashes`, so a subsequent
                // re-delivery of a block at the same hash is not short-circuited
                // as a "duplicate" against a rejected variant that never reached
                // any chain.
                //
                // If the receiver was dropped (the StateService is shutting
                // down), ignore the error: the lockout cannot matter once the
                // service exits.
                let _ = non_finalized_rejected_sender.send(child_hash);

                // Update the caller with the error.
                let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

                // Skip the things we only need to do for successfully committed blocks
                continue;
            }

            // Committing blocks to the finalized state keeps the same chain,
            // so we can update the chain seen by the rest of the application now.
            //
            // TODO: if this causes state request errors due to chain conflicts,
            //       fix the `service::read` bugs,
            //       or do the channel update after the finalized state commit
            let tip_block_height = update_latest_chain_channels(
                non_finalized_state,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            );

            // Update the caller with the result.
            let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

            while non_finalized_state
                .best_chain_len()
                .expect("just successfully inserted a non-finalized block above")
                > MAX_BLOCK_REORG_HEIGHT
            {
                tracing::trace!("finalizing block past the reorg limit");
                let contextually_verified_with_trees = non_finalized_state.finalize();
                prev_finalized_note_commitment_trees = finalized_state
                            .commit_finalized_direct(contextually_verified_with_trees, prev_finalized_note_commitment_trees.take(), "commit contextually-verified request")
                            .expect(
                                "unexpected finalized block commit error: note commitment and history trees were already checked by the non-finalized state",
                            ).1.into();
            }

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
