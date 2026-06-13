//! Writing blocks to the finalized and non-finalized states.
//!
//! The write pipeline has two stages:
//!
//! - the **worker** (Thread 1, this module's [`WriteBlockWorkerTask`]) commits
//!   each block to the in-memory non-finalized state, and
//! - the **disk writer** (Thread 2, [`disk_writer::DiskWriter`]) commits each
//!   durable-bound block to the finalized state (RocksDB).
//!
//! The worker acknowledges a checkpoint block as soon as it is in memory, so
//! block commits are not serialized behind disk I/O; a bounded channel limits
//! how many blocks have disk writes in flight.

mod disk_writer;
mod finalized_write_phase;

use std::{
    collections::VecDeque,
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
use zebra_chain::block::{self, Height};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    request::FinalizableBlock,
    service::{
        check,
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
        write::disk_writer::{DiskRequest, DiskWriter},
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
/// `disk_tip_height` is the height most recently published by the disk writer,
/// or [`NO_DISK_TIP_HEIGHT`] if nothing has been written yet. `inflight_disk`
/// holds the heights and hashes of the checkpoint-stream blocks handed to the
/// disk writer that are still in the non-finalized state, in commit order.
///
/// Pruning is **hash-pinned**: a block is finalized out of memory only when
/// its own height has been observed durable, and it is finalized by its exact
/// hash via [`finalize_root`](NonFinalizedState::finalize_root). This closes
/// the hole where a transient adversarial fork that briefly out-works the
/// pipeline chain could make a work-based prune pop the fork's root and orphan
/// the pipeline chain while its blocks are still being written. It also makes
/// the restored-backup property structural: only `inflight_disk` entries are
/// ever pruned, and those contain only worker-enqueued blocks, so restored
/// blocks (never enqueued) can never be pruned.
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
    disk_tip_height: &AtomicU32,
    inflight_disk: &mut VecDeque<(Height, block::Hash)>,
) {
    let disk_tip = disk_tip_height.load(Ordering::Acquire);
    if disk_tip == NO_DISK_TIP_HEIGHT {
        return;
    }

    let durable_height = Height(disk_tip);
    while let Some(&(height, hash)) = inflight_disk.front() {
        if height > durable_height {
            break;
        }

        non_finalized_state.finalize_root(Some(hash));
        inflight_disk.pop_front();
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

        // Block write pipeline: commit blocks to the non-finalized state
        // (Thread 1, this thread), then hand each durable-bound block to the
        // disk writer (Thread 2, see `disk_writer`). Thread 1 responds to the
        // state service as soon as a checkpoint block is in the non-finalized
        // state, so block commits are not serialized behind disk I/O; the
        // bounded channel limits how many blocks have disk writes in flight.
        //
        // Thread 1 does NOT publish the non-finalized state to the watch
        // channel while the checkpoint pipeline runs: keeping the chain `Arc`
        // uniquely owned lets each commit mutate it in place instead of
        // deep-cloning it, and keeps the backup task idle (as it is on the
        // non-pipelined path). In-flight blocks are not queryable through the
        // read service until their disk writes complete, at most a pipeline
        // depth later.
        //
        // The disk writer publishes the height of the block most recently
        // written to disk, so Thread 1 can prune the non-finalized state
        // without re-reading the finalized tip from the database on every
        // block. A reverse iterator over the height column family gets slower
        // as level 0 grows during the compaction-paused write phase, so
        // re-reading it per block would scale with sync depth. (The
        // `# Correctness` note for the atomic lives on the field in
        // [`disk_writer::DiskWriter`].)
        let disk_tip_height = Arc::new(AtomicU32::new(
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

        // The disk writer lives for the whole worker lifetime, inside one
        // thread scope. Both the checkpoint phase and the non-finalized phase
        // below hand blocks to it; the scope join propagates its panics.
        std::thread::scope(|s| {
            let (disk_tx, disk_rx) = crossbeam_channel::bounded::<DiskRequest>(pipeline_capacity);

            // Thread 2: the persistent disk writer, the sole caller of
            // `commit_finalized_direct`.
            let disk_writer = DiskWriter {
                receiver: disk_rx,
                finalized_state: finalized_state.clone(),
                disk_tip_height: disk_tip_height.clone(),
            };
            s.spawn(move || disk_writer.run());

            // The disk frontier: the next height to hand to the disk writer,
            // and the hash of the last block handed off. Checkpoint blocks are
            // handed off in strict parent-linked order, so the worker tracks
            // this itself instead of re-reading the finalized tip.
            let mut next_disk_height = finalized_state
                .db
                .finalized_tip_height()
                .map(|height| (height + 1).expect("committed heights are valid"))
                .unwrap_or(Height(0));
            let mut disk_frontier_hash = finalized_state.db.finalized_tip_hash();

            // Checkpoint-stream blocks handed to the disk writer that are still
            // in the non-finalized state awaiting durability. Pruned as the
            // disk writer reports each one durable. Genesis and overflow roots
            // are not recorded here (genesis is never in the NFS; overflow
            // roots are popped synchronously under a blocking ack).
            let mut inflight_disk: VecDeque<(Height, block::Hash)> = VecDeque::new();

            // Phase 1 (checkpoint pipeline): commit checkpoint-verified blocks
            // to the non-finalized state in height order and hand them to the
            // disk writer, until the state closes the finalized block channel.
            let phase1_shutdown = Self::run_checkpoint_phase(
                finalized_block_write_receiver,
                finalized_state,
                non_finalized_state,
                invalid_block_reset_sender,
                chain_tip_sender,
                &disk_tx,
                &disk_tip_height,
                &mut next_disk_height,
                &mut disk_frontier_hash,
                &mut inflight_disk,
            );

            if phase1_shutdown {
                // Dropping the sender drains and joins the disk writer.
                drop(disk_tx);
                return;
            }

            // The checkpoint bulk-write phase is over. Tell the disk writer to
            // drop its bulk-write guard (resume compaction, flush WAL-skipped
            // writes), and drop the recently-finalized cache. This is the
            // surviving responsibility of the old "flip": a reversible message
            // instead of an irreversible channel close.
            let _ = disk_tx.send(DiskRequest::EndBulk);
            non_finalized_state.disable_pruned_chain();

            // All the pipelined blocks are on disk: remove them from the
            // non-finalized state, so the full-verification phase starts from
            // an empty non-finalized state at the finalized tip.
            //
            // Only blocks the disk writer handled are removed (tracked in
            // `inflight_disk`): if the non-finalized state was restored from a
            // backup at startup (so the pipeline never ran), its blocks were
            // never handed off, so they are preserved for the full-verification
            // phase.
            Self::drain_inflight_disk(non_finalized_state, &disk_tip_height, &mut inflight_disk);

            // Do this check even if the channel got closed before any finalized
            // blocks were sent. This can happen if we're past the finalized tip.
            if invalid_block_reset_sender.is_closed() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                drop(disk_tx);
                return;
            }

            // Phase 2 (full verification): validate and commit
            // semantically-verified blocks, and handle invalidate/reconsider.
            Self::run_non_finalized_phase(
                non_finalized_block_write_receiver,
                finalized_state,
                non_finalized_state,
                non_finalized_rejected_sender,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
                &disk_tx,
                &mut next_disk_height,
                &mut disk_frontier_hash,
            );

            // Dropping the sender drains and joins the disk writer: blocks
            // already acked to the engine reach disk, the guard drops, and the
            // scope join re-raises any disk-writer panic.
            drop(disk_tx);
        });

        // We're finished receiving blocks from the state, and the disk writer
        // has drained and joined, so we can force the database to shut down.
        finalized_state.db.shutdown(true);
        std::mem::drop(self.finalized_state);
    }

    /// Runs the checkpoint pipeline phase: commits checkpoint-verified blocks
    /// to the non-finalized state in height order and hands them to the disk
    /// writer, until the state closes the finalized block channel.
    ///
    /// Returns `true` if the state service is shutting down.
    #[allow(clippy::too_many_arguments)]
    fn run_checkpoint_phase(
        finalized_block_write_receiver: &mut UnboundedReceiver<QueuedCheckpointVerified>,
        finalized_state: &mut FinalizedState,
        non_finalized_state: &mut NonFinalizedState,
        invalid_block_reset_sender: &mut UnboundedSender<block::Hash>,
        chain_tip_sender: &mut ChainTipSender,
        disk_tx: &crossbeam_channel::Sender<DiskRequest>,
        disk_tip_height: &AtomicU32,
        next_disk_height: &mut Height,
        disk_frontier_hash: &mut block::Hash,
        inflight_disk: &mut VecDeque<(Height, block::Hash)>,
    ) -> bool {
        while let Some((checkpoint_verified, rsp_tx)) =
            finalized_block_write_receiver.blocking_recv()
        {
            if invalid_block_reset_sender.is_closed() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                return true;
            }

            // Discard any children of invalid blocks in the channel.
            //
            // The pipeline requires blocks in height order. So if there has
            // been a block commit error, we need to drop all the descendants of
            // that block, until we receive a block at the required next height.
            if checkpoint_verified.height != *next_disk_height {
                debug!(
                    next_expected_height = ?next_disk_height,
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
            // non-finalized state's chains initialize their tree states from a
            // finalized tip, which doesn't exist yet. Route it through the disk
            // writer with a blocking ack (the channel is empty at that moment,
            // so this is serial), then advance the frontier.
            if checkpoint_verified.height == Height(0) {
                let hash = checkpoint_verified.hash;
                let tip_block = ChainTipBlock::from(checkpoint_verified.clone());
                let (ack_tx, ack_rx) = oneshot::channel();
                let request = DiskRequest::Write {
                    block: Box::new(FinalizableBlock::Checkpoint {
                        checkpoint_verified,
                    }),
                    bulk: true,
                    ack: Some(ack_tx),
                };
                if disk_tx.send(request).is_err() {
                    // The disk writer panicked; exiting unwinds the scope.
                    return true;
                }

                match ack_rx.blocking_recv() {
                    Ok(Ok(_hash)) => {
                        chain_tip_sender.set_finalized_tip(tip_block);
                        *next_disk_height =
                            (*next_disk_height + 1).expect("committed heights are valid");
                        *disk_frontier_hash = hash;
                        let _ = rsp_tx.send(Ok(hash));
                    }
                    Ok(Err(error)) => {
                        // Genesis disk-write error: report it, reset the queue
                        // to the (empty) finalized tip, and keep going.
                        metrics::counter!("state.checkpoint.error.block.count").increment(1);
                        metrics::gauge!("state.checkpoint.error.block.height")
                            .set(Height(0).0 as f64);

                        let finalized_tip = finalized_state.db.tip();
                        info!(
                            ?error,
                            last_valid_height = ?finalized_tip.map(|tip| tip.0),
                            last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                            "committing the genesis block to the finalized state failed, \
                             resetting state queue",
                        );

                        let _ = rsp_tx.send(Err(error));

                        if invalid_block_reset_sender
                            .send(finalized_state.db.finalized_tip_hash())
                            .is_err()
                        {
                            info!(
                                "StateService closed the block reset channel. \
                                 Is Zebra shutting down?"
                            );
                            return true;
                        }
                    }
                    Err(_recv_error) => {
                        // The disk writer dropped the ack sender: it panicked.
                        return true;
                    }
                }

                continue;
            }

            let hash = checkpoint_verified.hash;

            // Commit the block to the non-finalized state: a fast, in-memory
            // commit that updates the trees, nullifiers, and UTXOs without the
            // disk write. It also returns the finalizable block, with its
            // treestate already computed during the chain update and its spent
            // UTXOs already resolved by the commit.
            let (tip_block, finalizable) = non_finalized_state
                .commit_checkpoint_block(checkpoint_verified, &finalized_state.db)
                .expect(
                    "checkpoint block commits to the non-finalized state can't fail: \
                     the checkpoint hash chain pins the block's contents, and the \
                     database the chain context is read from is consistent",
                );

            chain_tip_sender.set_best_non_finalized_tip(tip_block);

            // Hand the just-committed block to the disk writer. Blocking on the
            // full bounded channel IS the pipeline backpressure. A SendError
            // means the disk writer panicked; exiting unwinds the scope.
            let request = DiskRequest::Write {
                block: Box::new(finalizable),
                bulk: true,
                ack: None,
            };
            if disk_tx.send(request).is_err() {
                return true;
            }
            inflight_disk.push_back((*next_disk_height, hash));
            *disk_frontier_hash = hash;

            // Respond to the state service now: the block is committed in
            // memory, and the disk write is in flight (it can only fail by
            // panicking).
            let _ = rsp_tx.send(Ok(hash));

            // The disk writer publishes its tip height, so this doesn't re-read
            // the finalized tip from the database on every block.
            prune_finalized_blocks(non_finalized_state, disk_tip_height, inflight_disk);

            *next_disk_height = (*next_disk_height + 1).expect("committed heights are valid");
        }

        false
    }

    /// Runs the full-verification phase: validates and commits
    /// semantically-verified blocks, handles invalidate/reconsider requests,
    /// and finalizes blocks past the reorg limit through the disk writer.
    #[allow(clippy::too_many_arguments)]
    fn run_non_finalized_phase(
        non_finalized_block_write_receiver: &mut UnboundedReceiver<NonFinalizedWriteMessage>,
        finalized_state: &mut FinalizedState,
        non_finalized_state: &mut NonFinalizedState,
        non_finalized_rejected_sender: &mut UnboundedSender<block::Hash>,
        chain_tip_sender: &mut ChainTipSender,
        non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
        backup_dir_path: Option<&Path>,
        disk_tx: &crossbeam_channel::Sender<DiskRequest>,
        next_disk_height: &mut Height,
        disk_frontier_hash: &mut block::Hash,
    ) {
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
                    backup_dir_path,
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
                backup_dir_path,
            );

            // Update the caller with the result.
            let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

            // Finalize any blocks past the reorg limit, through the disk writer.
            // The blocking ack preserves visibility: the pre-pop published
            // snapshot still holds the root, and the next publish happens only
            // after durability.
            while non_finalized_state
                .best_chain_len()
                .expect("just successfully inserted a non-finalized block above")
                > MAX_BLOCK_REORG_HEIGHT
            {
                tracing::trace!("finalizing block past the reorg limit");
                let finalizable = non_finalized_state.finalize();
                let height = finalizable.height();
                let hash = finalizable.hash();

                let (ack_tx, ack_rx) = oneshot::channel();
                let request = DiskRequest::Write {
                    block: Box::new(finalizable),
                    // `false`: drop the bulk guard if it somehow survived
                    // (belt-and-braces with EndBulk).
                    bulk: false,
                    ack: Some(ack_tx),
                };
                if disk_tx.send(request).is_err() {
                    // The disk writer panicked; exiting unwinds the scope.
                    return;
                }

                ack_rx
                    .blocking_recv()
                    .expect("disk writer is alive: it only exits when the worker drops disk_tx")
                    .expect(
                        "unexpected finalized block commit error: note commitment and history \
                         trees were already checked by the non-finalized state",
                    );

                *next_disk_height = (height + 1).expect("committed heights are valid");
                *disk_frontier_hash = hash;
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
    }

    /// Prunes every still-inflight checkpoint block the disk writer has
    /// finalized from the non-finalized state, at the end of the checkpoint
    /// phase.
    fn drain_inflight_disk(
        non_finalized_state: &mut NonFinalizedState,
        disk_tip_height: &AtomicU32,
        inflight_disk: &mut VecDeque<(Height, block::Hash)>,
    ) {
        prune_finalized_blocks(non_finalized_state, disk_tip_height, inflight_disk);
    }
}
