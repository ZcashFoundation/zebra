//! The block write worker (Thread 1 of the write pipeline).
//!
//! One persistent loop reads [`WriteMessage`]s from a single channel and
//! commits each block to the in-memory non-finalized state, then hands
//! durable-bound blocks to the [disk writer](super::disk_writer). Checkpoint
//! and semantic blocks are processed in arrival order; the single-threaded
//! `StateService` serializes both streams into commit order before they reach
//! this channel, so a FIFO read preserves parent-before-child ordering with no
//! cross-channel races and no locks.
//!
//! # Error policy
//!
//! | Path | Handling |
//! |---|---|
//! | Genesis disk write error (ack path) | error metrics + reset(db tip); continue |
//! | Checkpoint in-memory commit error | respond Err + reset(parent); NFS untouched; continue |
//! | `Chain::push` after all checks passed | expect-panic (internal invariant) |
//! | Post-ack checkpoint disk error (ack: None) | disk writer panics (documented fatal) |
//! | Overflow-root disk error (ack: Some) | worker expect-panic (trees already validated) |
//! | Semantic contextual error | respond Err + parent_error_map + rejected channel |
//! | Input/reset channel close, disk SendError | clean break; drain; scope join re-raises |

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use indexmap::IndexMap;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    watch,
};

use zebra_chain::block::{self, Height};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    error::{CommitBlockError, CommitCheckpointVerifiedError},
    request::FinalizableBlock,
    service::{
        finalized_state::FinalizedState,
        non_finalized_state::NonFinalizedState,
        write::{
            disk_writer::{DiskRequest, DiskWriter},
            update_latest_chain_channels, validate_and_commit_non_finalized, WriteMessage,
            MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS, NO_DISK_TIP_HEIGHT, PARENT_ERROR_MAP_LIMIT,
        },
        ChainTipBlock, ChainTipSender, InvalidateError, ReconsiderError,
    },
    ValidateContextError,
};

/// The block write worker: a single loop committing blocks to the
/// non-finalized state and handing durable-bound blocks to the disk writer.
pub(super) struct WriteBlockWorker {
    /// The single input channel of blocks and admin requests from the state.
    pub(super) receiver: UnboundedReceiver<WriteMessage>,

    /// The finalized state, shared with the disk writer (the database handle
    /// is cloned, the in-memory commit reads trees and value pools from it).
    pub(super) finalized_state: FinalizedState,

    /// The in-memory non-finalized state, owned by this thread.
    pub(super) non_finalized_state: NonFinalizedState,

    /// On a checkpoint commit failure or genesis disk error, the hash of the
    /// last valid block, so the state rewinds and re-drains.
    pub(super) invalid_block_reset_sender: UnboundedSender<block::Hash>,

    /// The hash of every non-finalized block this worker rejected, so the
    /// state can drop it from its sent-hash set (otherwise an honest
    /// re-delivery at the same hash is locked out as a "duplicate").
    pub(super) non_finalized_rejected_sender: UnboundedSender<block::Hash>,

    /// Publishes the latest chain tip to the rest of the application.
    pub(super) chain_tip_sender: ChainTipSender,

    /// Publishes non-finalized state snapshots to the read service and mempool.
    pub(super) non_finalized_state_sender: watch::Sender<NonFinalizedState>,

    /// If `Some`, the non-finalized state is written to this backup directory
    /// synchronously before each channel update, instead of via the async
    /// backup task.
    pub(super) backup_dir_path: Option<PathBuf>,

    /// When the last checkpoint-commit-failure warning was logged, used to
    /// rate-limit the warning (a peer can drive that failure path).
    pub(super) last_commit_error_warn: std::time::Instant,
}

/// Log at most one checkpoint-commit-failure warning per this interval.
const COMMIT_ERROR_WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

impl WriteBlockWorker {
    /// Reads blocks from the channel, commits them to the non-finalized state,
    /// hands durable-bound blocks to the disk writer, and updates the chain tip
    /// and non-finalized state channels.
    #[tracing::instrument(
        level = "debug",
        skip(self),
        fields(network = %self.non_finalized_state.network)
    )]
    pub(super) fn run(mut self) {
        // The spendable-output cache retention window for the checkpoint
        // bulk-write phase. Configured values below the minimum are raised.
        let retained_blocks = {
            let configured = self
                .finalized_state
                .db
                .config()
                .checkpoint_sync_retained_blocks;
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

        // How far the disk writes may lag the in-memory commits. Configured
        // values below the minimum are raised.
        let pipeline_capacity = {
            let configured = self
                .finalized_state
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

        // The disk-writer tip height, published by the disk writer and read by
        // this thread's prune. Initialized to the on-disk tip, or the
        // no-blocks sentinel for an empty database.
        let disk_tip_height = Arc::new(AtomicU32::new(
            self.finalized_state
                .db
                .finalized_tip_height()
                .map_or(NO_DISK_TIP_HEIGHT, |height| height.0),
        ));

        // The disk writer lives for the worker's whole lifetime inside one
        // thread scope. The scope join propagates the disk writer's panics.
        std::thread::scope(|s| {
            let (disk_tx, disk_rx) = crossbeam_channel::bounded::<DiskRequest>(pipeline_capacity);

            let disk_writer = DiskWriter {
                receiver: disk_rx,
                finalized_state: self.finalized_state.clone(),
                disk_tip_height: disk_tip_height.clone(),
            };
            s.spawn(move || disk_writer.run());

            // Worker-local commit/disk-frontier state, no synchronization.
            let mut loop_state = WorkerLoopState {
                // The disk frontier: the next height to hand to the disk writer
                // and the hash of the last block handed off. Checkpoint blocks
                // are handed off in strict parent-linked order, so the worker
                // tracks this itself instead of re-reading the finalized tip.
                next_disk_height: self
                    .finalized_state
                    .db
                    .finalized_tip_height()
                    .map(|height| (height + 1).expect("committed heights are valid"))
                    .unwrap_or(Height(0)),
                disk_frontier_hash: self.finalized_state.db.finalized_tip_hash(),
                // Checkpoint-stream blocks handed to the disk writer that are
                // still in the non-finalized state awaiting durability. Genesis
                // and overflow roots are not recorded here.
                inflight_disk: VecDeque::new(),
                // Whether the bulk-write mode (PrunedChain cache + disk-writer
                // guard) is active. Enabled lazily on the first checkpoint
                // commit, disabled at the bulk→semantic transition.
                bulk_active: false,
                // Errors propagated down to queued child blocks.
                parent_error_map: IndexMap::new(),
                retained_blocks,
                disk_tip_height: &disk_tip_height,
                disk_tx: &disk_tx,
            };

            while let Some(msg) = self.receiver.blocking_recv() {
                // The state closed the reset channel: it is shutting down.
                if self.invalid_block_reset_sender.is_closed() {
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    break;
                }

                let shutting_down =
                    match msg {
                        WriteMessage::Checkpoint((checkpoint_verified, rsp_tx)) => self
                            .handle_checkpoint_block(&mut loop_state, checkpoint_verified, rsp_tx),
                        WriteMessage::Semantic((semantically_verified, rsp_tx)) => self
                            .handle_semantic_block(&mut loop_state, semantically_verified, rsp_tx),
                        WriteMessage::Invalidate { hash, rsp_tx } => {
                            self.handle_invalidate(&loop_state, hash, rsp_tx);
                            false
                        }
                        WriteMessage::Reconsider { hash, rsp_tx } => {
                            self.handle_reconsider(&loop_state, hash, rsp_tx);
                            false
                        }
                    };

                if shutting_down {
                    break;
                }
            }

            // Dropping the sender drains and joins the disk writer: blocks
            // already acked reach disk, the guard drops, and the scope join
            // re-raises any disk-writer panic.
            drop(disk_tx);
        });

        // The disk writer has drained and joined, so force the database to
        // shut down.
        self.finalized_state.db.shutdown(true);
        std::mem::drop(self.finalized_state);
    }

    /// Commits a checkpoint-verified block to the non-finalized state and hands
    /// it to the disk writer.
    ///
    /// Returns `true` if the state service is shutting down.
    fn handle_checkpoint_block(
        &mut self,
        loop_state: &mut WorkerLoopState<'_>,
        checkpoint_verified: crate::CheckpointVerifiedBlock,
        rsp_tx: tokio::sync::oneshot::Sender<
            Result<block::Hash, crate::error::CommitCheckpointVerifiedError>,
        >,
    ) -> bool {
        let height = checkpoint_verified.height;
        let hash = checkpoint_verified.hash;
        let parent_hash = checkpoint_verified.block.header.previous_block_hash;

        // Genesis must be committed directly to disk: the non-finalized state's
        // chains initialize their tree states from a finalized tip, which
        // doesn't exist yet. Route it through the disk writer with a blocking
        // ack (the channel is empty at that moment, so this is serial).
        if height == Height(0) && self.finalized_state.db.is_empty() {
            return self.commit_genesis(loop_state, checkpoint_verified, rsp_tx);
        }

        // Locate the parent chain by tip hash. In pure checkpoint sync this is
        // the single chain and always matches; the fallback arms cost nothing
        // on the hot path.
        let parent_is_a_chain_tip = self
            .non_finalized_state
            .find_chain(|chain| chain.non_finalized_tip_hash() == parent_hash)
            .is_some();
        let parent_is_finalized_tip = parent_hash == self.finalized_state.db.finalized_tip_hash();

        if !parent_is_a_chain_tip && !parent_is_finalized_tip {
            // No chain extends this block's parent.

            // Adopt-twin: the block may already have entered memory via a
            // semantic commit (a handoff-window race). If any chain holds this
            // exact block, the ack contract — committed in memory, disk write
            // in flight or future — is already satisfied: respond Ok and let
            // its disk handoff happen when its checkpoint child arrives (or it
            // finalizes at depth 1000 as an ordinary non-finalized block).
            if self
                .non_finalized_state
                .find_chain(|chain| chain.height_by_hash(hash) == Some(height))
                .is_some()
            {
                let _ = rsp_tx.send(Ok(hash));
                return false;
            }

            // Stale, ahead, or sibling: reject explicitly. The engine resubmits
            // its retained copy above the frontier on any error. No reset: it
            // could overwrite a valid sent hash.
            let next_height = self.canonical_next_height(loop_state);
            let _ = rsp_tx.send(Err(CommitBlockError::OutOfOrder {
                height,
                next_height,
            }
            .into()));
            return false;
        }

        // Flush any semantically-committed ancestors of this block that haven't
        // been handed to the disk writer yet (needed for any-order: a semantic
        // block extended the chain past the disk frontier before this
        // checkpoint child arrived). Fails safe on a fork below the frontier.
        if height > loop_state.next_disk_height
            && !self.flush_unhanded_ancestors(loop_state, parent_hash, height)
        {
            let next_height = self.canonical_next_height(loop_state);
            let _ = rsp_tx.send(Err(CommitBlockError::OutOfOrder {
                height,
                next_height,
            }
            .into()));
            return false;
        }

        // Lazily enter bulk-write mode on the first checkpoint commit: enable
        // the recently-finalized cache. The disk-writer guard is created by the
        // first bulk write below.
        if !loop_state.bulk_active {
            self.non_finalized_state
                .enable_pruned_chain(loop_state.retained_blocks);
            loop_state.bulk_active = true;
        }

        // Commit the block to the non-finalized state: a fast, in-memory commit
        // that updates the trees, nullifiers, and UTXOs without the disk write.
        // It also returns the finalizable block, with its treestate already
        // computed during the chain update and its spent UTXOs already resolved.
        let (tip_block, finalizable) = match self
            .non_finalized_state
            .commit_checkpoint_block(checkpoint_verified, &self.finalized_state.db)
        {
            Ok(committed) => committed,
            Err(error) => {
                // A peer-influenceable failure: the NU5+ auth-data
                // hashBlockCommitments check (a substituted-signatures block
                // passes every engine check and fails only here), or the
                // spend/value-balance construction. The non-finalized state is
                // untouched (validate-before-mutate), so the resent copy and
                // all retained descendants recommit cleanly.
                //
                // Reset the service to the parent (the last acked block): it
                // rewinds and re-drains; descendants arrive above the canonical
                // tip and get OutOfOrder, which the engine answers by
                // resubmitting from retained copies — no re-download.
                //
                // Rate-limit the warning: a peer can drive this path, so an
                // unthrottled log is a noise amplifier.
                if self.last_commit_error_warn.elapsed() >= COMMIT_ERROR_WARN_INTERVAL {
                    self.last_commit_error_warn = std::time::Instant::now();
                    warn!(
                        ?error,
                        ?height,
                        ?hash,
                        "checkpoint block failed its in-memory commit; \
                         resetting the queue to the parent and recovering",
                    );
                }
                let _ = rsp_tx.send(Err(error.into()));
                let _ = self.invalid_block_reset_sender.send(parent_hash);
                return false;
            }
        };

        // Only update the best-chain tip if the committed chain is the best
        // chain (always true in pure bulk; correct under forks).
        if self
            .non_finalized_state
            .best_chain()
            .is_some_and(|chain| chain.non_finalized_tip_hash() == hash)
        {
            self.chain_tip_sender.set_best_non_finalized_tip(tip_block);
        }

        // Hand the just-committed block to the disk writer. Blocking on the full
        // bounded channel IS the pipeline backpressure. A SendError means the
        // disk writer panicked; exiting unwinds the scope.
        if self
            .enqueue_for_disk(loop_state, finalizable, true, None)
            .is_err()
        {
            return true;
        }

        // Respond now: the block is committed in memory, and the disk write is
        // in flight (it can only fail by panicking).
        let _ = rsp_tx.send(Ok(hash));

        // The disk writer publishes its tip height, so this doesn't re-read the
        // finalized tip from the database on every block.
        prune_durable_blocks(
            &mut self.non_finalized_state,
            loop_state.disk_tip_height,
            &mut loop_state.inflight_disk,
        );

        false
    }

    /// The next height the worker can accept on the canonical chain: the
    /// canonical tip + 1, or `next_disk_height` when the non-finalized state is
    /// empty.
    fn canonical_next_height(&self, loop_state: &WorkerLoopState<'_>) -> Height {
        self.non_finalized_state
            .best_chain()
            .and_then(|chain| chain.non_finalized_tip().0 + 1)
            .unwrap_or(loop_state.next_disk_height)
    }

    /// Sends a block to the disk writer and advances the disk frontier, without
    /// recording it in `inflight_disk`.
    ///
    /// Used for blocks that are popped from the non-finalized state at handoff
    /// (genesis and overflow roots), so prune never needs to retire them.
    ///
    /// `ack` is `Some` for senders that block on durability and own the error
    /// policy. Returns `Err` if the disk writer's channel is closed (it
    /// panicked).
    #[allow(clippy::result_unit_err)]
    fn send_to_disk(
        &self,
        loop_state: &mut WorkerLoopState<'_>,
        finalizable: FinalizableBlock,
        bulk: bool,
        ack: Option<
            tokio::sync::oneshot::Sender<Result<block::Hash, CommitCheckpointVerifiedError>>,
        >,
    ) -> Result<(), ()> {
        let height = finalizable.height();
        let hash = finalizable.hash();

        loop_state
            .disk_tx
            .send(DiskRequest::Write {
                block: Box::new(finalizable),
                bulk,
                ack,
            })
            .map_err(|_| ())?;

        loop_state.next_disk_height = (height + 1).expect("committed heights are valid");
        loop_state.disk_frontier_hash = hash;

        Ok(())
    }

    /// Hands a block that stays in the non-finalized state to the disk writer,
    /// recording it in `inflight_disk` so prune retires it once durable, and
    /// advancing the disk frontier.
    ///
    /// `ack` is `None` for the fire-and-forget checkpoint stream (whose
    /// post-ack disk errors are fatal). Returns `Err` if the disk writer's
    /// channel is closed (it panicked).
    #[allow(clippy::result_unit_err)]
    fn enqueue_for_disk(
        &self,
        loop_state: &mut WorkerLoopState<'_>,
        finalizable: FinalizableBlock,
        bulk: bool,
        ack: Option<
            tokio::sync::oneshot::Sender<Result<block::Hash, CommitCheckpointVerifiedError>>,
        >,
    ) -> Result<(), ()> {
        let height = finalizable.height();
        let hash = finalizable.hash();

        self.send_to_disk(loop_state, finalizable, bulk, ack)?;
        loop_state.inflight_disk.push_back((height, hash));

        Ok(())
    }

    /// Hands the parent chain's semantically-committed ancestors at heights
    /// `next_disk_height ..= height - 1` to the disk writer, in order, before
    /// their checkpoint child is committed.
    ///
    /// These ancestors were fully contextually validated when committed
    /// (strictly more than checkpoint blocks get), and the engine's
    /// convert-time linkage pins every ancestor hash to the known-hash list.
    ///
    /// Returns `false` (fail safe, no mutation) if the chain forked below the
    /// disk frontier — never force-finalize across competing forks.
    fn flush_unhanded_ancestors(
        &mut self,
        loop_state: &mut WorkerLoopState<'_>,
        parent_hash: block::Hash,
        height: Height,
    ) -> bool {
        // Find the parent chain (the one whose tip is the block's parent).
        let Some(parent_chain) = self
            .non_finalized_state
            .find_chain(|chain| chain.non_finalized_tip_hash() == parent_hash)
        else {
            return false;
        };

        // Verify the chain's block at the frontier links to the frontier hash;
        // if not, it forked below the frontier and we must not flush across it.
        let frontier_block = parent_chain.blocks.get(&loop_state.next_disk_height);
        let Some(frontier_block) = frontier_block else {
            return false;
        };
        if frontier_block.block.header.previous_block_hash != loop_state.disk_frontier_hash {
            return false;
        }

        // Hand each ancestor at next_disk_height ..= height - 1 to the disk
        // writer, non-popping (the blocks stay in the NFS until prune observes
        // durability, so there is no visibility gap and no acks are needed).
        let mut flush_height = loop_state.next_disk_height;
        while flush_height < height {
            let block = parent_chain
                .blocks
                .get(&flush_height)
                .expect("ancestor heights are contiguous up to the committed block");
            let treestate = parent_chain
                .treestate(flush_height.into())
                .expect("the treestate exists for an ancestor in the chain");
            let finalizable = FinalizableBlock::new(block.clone(), treestate);

            if self
                .enqueue_for_disk(loop_state, finalizable, true, None)
                .is_err()
            {
                // The disk writer panicked; the caller exits and unwinds.
                return false;
            }

            flush_height = (flush_height + 1).expect("committed heights are valid");
        }

        true
    }

    /// Commits the genesis block directly through the disk writer with a
    /// blocking ack.
    ///
    /// Returns `true` if the state service is shutting down.
    fn commit_genesis(
        &mut self,
        loop_state: &mut WorkerLoopState<'_>,
        checkpoint_verified: crate::CheckpointVerifiedBlock,
        rsp_tx: tokio::sync::oneshot::Sender<
            Result<block::Hash, crate::error::CommitCheckpointVerifiedError>,
        >,
    ) -> bool {
        let hash = checkpoint_verified.hash;
        let tip_block = ChainTipBlock::from(checkpoint_verified.clone());
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        if loop_state
            .disk_tx
            .send(DiskRequest::Write {
                block: Box::new(FinalizableBlock::Checkpoint {
                    checkpoint_verified,
                }),
                bulk: true,
                ack: Some(ack_tx),
            })
            .is_err()
        {
            return true;
        }

        match ack_rx.blocking_recv() {
            Ok(Ok(_hash)) => {
                self.chain_tip_sender.set_finalized_tip(tip_block);
                loop_state.next_disk_height =
                    (loop_state.next_disk_height + 1).expect("committed heights are valid");
                loop_state.disk_frontier_hash = hash;
                let _ = rsp_tx.send(Ok(hash));
                false
            }
            Ok(Err(error)) => {
                // Genesis disk-write error: report it, reset the queue to the
                // (empty) finalized tip, and keep going.
                metrics::counter!("state.checkpoint.error.block.count").increment(1);
                metrics::gauge!("state.checkpoint.error.block.height").set(Height(0).0 as f64);

                let finalized_tip = self.finalized_state.db.tip();
                info!(
                    ?error,
                    last_valid_height = ?finalized_tip.map(|tip| tip.0),
                    last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                    "committing the genesis block to the finalized state failed, \
                     resetting state queue",
                );

                let _ = rsp_tx.send(Err(error));

                if self
                    .invalid_block_reset_sender
                    .send(self.finalized_state.db.finalized_tip_hash())
                    .is_err()
                {
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    return true;
                }
                false
            }
            // The disk writer dropped the ack sender: it panicked.
            Err(_recv_error) => true,
        }
    }

    /// Validates and commits a semantically-verified block to the non-finalized
    /// state, publishing the new state and finalizing past the reorg limit.
    ///
    /// Returns `true` if the state service is shutting down.
    fn handle_semantic_block(
        &mut self,
        loop_state: &mut WorkerLoopState<'_>,
        queued_child: crate::SemanticallyVerifiedBlock,
        rsp_tx: tokio::sync::oneshot::Sender<
            Result<block::Hash, crate::CommitSemanticallyVerifiedError>,
        >,
    ) -> bool {
        // Retire any already-durable checkpoint blocks before deciding how to
        // commit this block: a semantic block whose parent is the finalized
        // tip must commit as a fresh chain, but if a durable checkpoint block
        // is still in the non-finalized state (its prune lagged the disk
        // write), the commit would fork a sibling chain that the post-commit
        // prune then drops. Pruning first keeps the non-finalized state
        // consistent with durability.
        prune_durable_blocks(
            &mut self.non_finalized_state,
            loop_state.disk_tip_height,
            &mut loop_state.inflight_disk,
        );

        let child_hash = queued_child.hash;
        let parent_hash = queued_child.block.header.previous_block_hash;
        let parent_error = loop_state.parent_error_map.get(&parent_hash);

        // If the parent block was marked rejected, reject all its children too:
        // all consensus rules were checked before committing the failing
        // ancestor, so its descendants are known invalid.
        let result = if let Some(parent_error) = parent_error {
            Err(parent_error.clone())
        } else {
            tracing::trace!(?child_hash, "validating queued child");
            validate_and_commit_non_finalized(
                &self.finalized_state.db,
                &mut self.non_finalized_state,
                queued_child,
            )
        };

        // TODO: fix the test timing bugs that require the result to be sent
        //       after `update_latest_chain_channels()`, and send it here

        if let Err(ref error) = result {
            // Mark any descendant blocks as rejected.
            loop_state
                .parent_error_map
                .insert(child_hash, error.clone());

            // Bound the error map.
            if loop_state.parent_error_map.len() > PARENT_ERROR_MAP_LIMIT {
                loop_state.parent_error_map.shift_remove_index(0);
            }

            // Signal the state to drop this hash from its sent set, so a later
            // honest re-delivery at the same hash isn't short-circuited as a
            // "duplicate" against a rejected variant that never reached a chain.
            // If the receiver was dropped (shutting down), the lockout can't
            // matter once the service exits.
            let _ = self.non_finalized_rejected_sender.send(child_hash);

            let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));
            return false;
        }

        // Retire any already-durable checkpoint blocks before publishing, so
        // the published snapshot never carries roots the disk writer has
        // finalized.
        prune_durable_blocks(
            &mut self.non_finalized_state,
            loop_state.disk_tip_height,
            &mut loop_state.inflight_disk,
        );

        // A semantic block has been committed, so the checkpoint bulk-write
        // phase is over for now: tell the disk writer to drop its bulk guard
        // (resume compaction, flush WAL-skipped writes) and drop the
        // recently-finalized cache. This is the flip's only surviving
        // responsibility, now a reversible message: a later checkpoint block
        // simply re-enables both (re-enabling the cache empty is always
        // correct; lookups fall back to database reads).
        if loop_state.bulk_active {
            let _ = loop_state.disk_tx.send(DiskRequest::EndBulk);
            self.non_finalized_state.disable_pruned_chain();
            loop_state.bulk_active = false;
        }

        // Committing keeps the same best chain, so publish it now.
        //
        // TODO: if this causes state request errors due to chain conflicts,
        //       fix the `service::read` bugs, or publish after the disk commit
        let tip_block_height = update_latest_chain_channels(
            &self.non_finalized_state,
            &mut self.chain_tip_sender,
            &self.non_finalized_state_sender,
            self.backup_dir_path.as_deref(),
        );

        let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

        // Finalize any blocks past the reorg limit, through the disk writer.
        // The blocking ack preserves visibility: the pre-pop published snapshot
        // still holds the root, and the next publish happens only after
        // durability.
        //
        // The `root >= next_disk_height` guard stops the loop at any in-flight
        // checkpoint-era root (those are retired by `prune_durable_blocks` once
        // durable, so finalizing them here would double-write).
        while self
            .non_finalized_state
            .best_chain_len()
            .expect("just successfully inserted a non-finalized block above")
            > MAX_BLOCK_REORG_HEIGHT
            && self
                .non_finalized_state
                .root_height()
                .is_some_and(|root| root >= loop_state.next_disk_height)
        {
            tracing::trace!("finalizing block past the reorg limit");
            let finalizable = self.non_finalized_state.finalize();

            // The root was popped out of the non-finalized state, so it is not
            // tracked in inflight_disk; the blocking ack reports any disk error
            // as fatal. `bulk: false` drops the guard if it somehow survived
            // (belt-and-braces with EndBulk).
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            if self
                .send_to_disk(loop_state, finalizable, false, Some(ack_tx))
                .is_err()
            {
                return true;
            }

            ack_rx
                .blocking_recv()
                .expect("disk writer is alive: it only exits when the worker drops disk_tx")
                .expect(
                    "unexpected finalized block commit error: note commitment and history \
                     trees were already checked by the non-finalized state",
                );
        }

        // Update the metrics now that semantic and contextual validation passed.
        metrics::counter!("state.full_verifier.committed.block.count").increment(1);
        metrics::counter!("zcash.chain.verified.block.total").increment(1);
        metrics::gauge!("state.full_verifier.committed.block.height")
            .set(tip_block_height.0 as f64);
        // Updated for both fully verified and checkpoint blocks; can't conflict
        // because this worker commits blocks in order.
        metrics::gauge!("zcash.chain.verified.block.height").set(tip_block_height.0 as f64);

        tracing::trace!("finished processing queued block");
        false
    }

    /// Invalidates a block and its descendants in the non-finalized state.
    ///
    /// A block whose disk write is enqueued, in flight, or complete (its height
    /// is below the disk frontier) can't be invalidated; above the frontier,
    /// invalidation works even during checkpoint sync.
    fn handle_invalidate(
        &mut self,
        loop_state: &WorkerLoopState<'_>,
        hash: block::Hash,
        rsp_tx: tokio::sync::oneshot::Sender<Result<block::Hash, InvalidateError>>,
    ) {
        if self.is_below_disk_frontier(loop_state, hash) {
            let _ = rsp_tx.send(Err(InvalidateError::ProcessingCheckpointedBlocks));
            return;
        }

        tracing::info!(?hash, "invalidating a block in the non-finalized state");
        let result = self.non_finalized_state.invalidate_block(hash);
        let succeeded = result.is_ok();
        let _ = rsp_tx.send(result);

        // Publish only on success: a failed invalidate didn't change the state.
        if succeeded {
            self.publish_after_admin_op();
        }
    }

    /// Reconsiders a previously invalidated block into the non-finalized state.
    ///
    /// A block whose disk write is enqueued, in flight, or complete (its height
    /// is below the disk frontier) can't be reconsidered; above the frontier,
    /// reconsideration works even during checkpoint sync.
    fn handle_reconsider(
        &mut self,
        loop_state: &WorkerLoopState<'_>,
        hash: block::Hash,
        rsp_tx: tokio::sync::oneshot::Sender<Result<Vec<block::Hash>, ReconsiderError>>,
    ) {
        if self.is_below_disk_frontier(loop_state, hash) {
            let _ = rsp_tx.send(Err(ReconsiderError::CheckpointCommitInProgress));
            return;
        }

        tracing::info!(?hash, "reconsidering a block in the non-finalized state");
        let result = self
            .non_finalized_state
            .reconsider_block(hash, &self.finalized_state.db);
        let succeeded = result.is_ok();
        let _ = rsp_tx.send(result);

        if succeeded {
            self.publish_after_admin_op();
        }
    }

    /// Returns `true` if `hash`'s height is below the disk frontier (its disk
    /// write is enqueued, in flight, or complete), so it can't be the target of
    /// an invalidate or reconsider.
    ///
    /// A hash that isn't in any non-finalized chain (already finalized, or
    /// unknown) is treated as below the frontier: invalidating it is either
    /// impossible or already covered by the gate.
    fn is_below_disk_frontier(&self, loop_state: &WorkerLoopState<'_>, hash: block::Hash) -> bool {
        match self.non_finalized_state.height_by_hash(hash) {
            Some(height) => height < loop_state.next_disk_height,
            None => true,
        }
    }

    /// Publishes the latest chain channels after a successful invalidate or
    /// reconsider, tolerating an emptied non-finalized state.
    ///
    /// Invalidating the only chain leaves the non-finalized state empty; in
    /// that case publish the empty snapshot and fall the chain tip back to the
    /// finalized tip. Readers already tolerate snapshots that drop blocks
    /// (reorg semantics).
    fn publish_after_admin_op(&mut self) {
        if self.non_finalized_state.best_chain().is_some() {
            update_latest_chain_channels(
                &self.non_finalized_state,
                &mut self.chain_tip_sender,
                &self.non_finalized_state_sender,
                self.backup_dir_path.as_deref(),
            );
            return;
        }

        // The non-finalized state is empty: publish the empty snapshot and fall
        // the chain tip back to the finalized tip.
        if let Some(backup_dir_path) = self.backup_dir_path.as_deref() {
            self.non_finalized_state.write_to_backup(backup_dir_path);
        }
        let _ = self
            .non_finalized_state_sender
            .send(self.non_finalized_state.clone());

        let finalized_tip = self
            .finalized_state
            .db
            .tip_block()
            .map(crate::CheckpointVerifiedBlock::from)
            .map(ChainTipBlock::from);
        self.chain_tip_sender.set_finalized_tip(finalized_tip);
    }
}

/// Worker-local commit/disk-frontier state threaded through the handlers.
///
/// Borrows the disk-writer channel and tip atomic, which live for the worker's
/// whole lifetime inside the thread scope.
struct WorkerLoopState<'scope> {
    /// The next height to hand to the disk writer.
    next_disk_height: Height,
    /// The hash of the last block handed to the disk writer.
    disk_frontier_hash: block::Hash,
    /// Checkpoint-stream blocks handed to the disk writer, still in the NFS
    /// awaiting durability, in commit order.
    inflight_disk: VecDeque<(Height, block::Hash)>,
    /// Whether bulk-write mode (PrunedChain cache + disk guard) is active.
    bulk_active: bool,
    /// Errors propagated down to queued child blocks.
    parent_error_map: IndexMap<block::Hash, ValidateContextError>,
    /// The recently-finalized cache retention window.
    retained_blocks: u32,
    /// The disk-writer tip height, published by the disk writer.
    disk_tip_height: &'scope AtomicU32,
    /// The channel to the disk writer.
    disk_tx: &'scope crossbeam_channel::Sender<DiskRequest>,
}

/// Prunes every still-inflight checkpoint block the disk writer has finalized
/// from the non-finalized state, keeping its memory use bounded by the pipeline
/// capacity.
///
/// `disk_tip_height` is the height most recently published by the disk writer,
/// or [`NO_DISK_TIP_HEIGHT`] if nothing has been written yet. `inflight_disk`
/// holds the heights and hashes of the checkpoint-stream blocks handed to the
/// disk writer that are still in the non-finalized state, in commit order.
///
/// Pruning is **hash-pinned**: a block is finalized out of memory only when its
/// own height has been observed durable, and it is finalized by its exact hash
/// via [`finalize_root`](NonFinalizedState::finalize_root). This closes the
/// hole where a transient adversarial fork that briefly out-works the pipeline
/// chain could make a work-based prune pop the fork's root and orphan the
/// pipeline chain while its blocks are still being written. It also makes the
/// restored-backup property structural: only `inflight_disk` entries are ever
/// pruned, and those hold only worker-enqueued blocks, so restored blocks
/// (never enqueued) can never be pruned.
///
/// While the recently-finalized cache is enabled, each pruned root leaves its
/// still-spendable outputs behind in memory, so checkpoint-sync spend lookups
/// avoid database point reads (see
/// [`PrunedChain`](crate::service::non_finalized_state::PrunedChain)).
///
/// # Correctness
///
/// The `Acquire` load pairs with the disk writer's `Release` store, which
/// happens **after** `commit_finalized_direct` returns: reading height `H` here
/// therefore happens-after the completion of `H`'s database write, so a block
/// is never pruned from the in-memory non-finalized state before its on-disk
/// copy is in place — readers always find the block in one of the two.
/// `Relaxed` would not document or guarantee that prune-after-write ordering
/// (it would lean on RocksDB's internal synchronization instead); the explicit
/// `Release`/`Acquire` pair makes the invariant independent of database
/// internals.
pub(super) fn prune_durable_blocks(
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
