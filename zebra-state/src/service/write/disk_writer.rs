//! The persistent disk-writer thread of the block write pipeline.
//!
//! The write worker (Thread 1, see [`worker`](super::worker)) commits blocks
//! to the in-memory non-finalized state and hands each durable-bound block to
//! this thread (Thread 2) over a bounded channel. This thread is the **only**
//! caller of [`FinalizedState::commit_finalized_direct`] in the whole system,
//! so every tip-linkage assertion, `debug_stop_at_height` exit, and
//! elasticsearch index goes through one place, in strict parent-linked commit
//! order.
//!
//! Decoupling the disk write from the in-memory commit lets the worker
//! acknowledge a checkpoint block as soon as it is in memory, so block commits
//! are not serialized behind disk I/O; the bounded channel limits how many
//! blocks have disk writes in flight (the pipeline depth).
//!
//! While checkpoint-stream blocks are being written, this thread holds a
//! [`FinalizedWritePhase`] guard that pauses RocksDB auto-compaction (and
//! optionally skips the write-ahead log). The guard is created on the first
//! bulk write and dropped on an [`DiskRequest::EndBulk`] message, a non-bulk
//! write, or channel close — a reversible, locally-decided state rather than
//! an irreversible channel close.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use crossbeam_channel::Receiver;
use tokio::sync::oneshot;

use zebra_chain::{block, parallel::tree::NoteCommitmentTrees};

use crate::{
    error::CommitCheckpointVerifiedError,
    request::FinalizableBlock,
    service::{finalized_state::FinalizedState, write::finalized_write_phase::FinalizedWritePhase},
};

/// A request to the disk writer.
pub(super) enum DiskRequest {
    /// Commit a block to the finalized state (RocksDB).
    Write {
        /// The block to commit, with its treestate already computed (for
        /// contextual blocks) or to be derived from the database tip (for
        /// checkpoint blocks).
        block: Box<FinalizableBlock>,

        /// `true` for checkpoint-stream and flushed-ancestor blocks: written
        /// under the bulk-write guard (compaction pause, optional WAL skip).
        ///
        /// `false` for genesis and steady-state overflow roots: these drop the
        /// guard so the database is back to its normal write mode.
        bulk: bool,

        /// When `Some`, the disk writer reports the write result instead of
        /// treating a write error as fatal. Used by genesis and the overflow
        /// finalization path, whose senders block on the ack.
        ///
        /// When `None`, a write error is a fatal invariant violation (the
        /// block was already acknowledged to its submitter after the in-memory
        /// commit and may have been observed via the chain tip), so the disk
        /// writer panics; see the loop below.
        ack: Option<oneshot::Sender<Result<block::Hash, CommitCheckpointVerifiedError>>>,
    },

    /// The bulk write phase is over: drop the [`FinalizedWritePhase`] guard,
    /// flushing WAL-skipped writes and resuming auto-compaction.
    ///
    /// FIFO-ordered behind every write it covers, so the guard never drops
    /// before the writes it was protecting reach disk.
    EndBulk,
}

/// The persistent disk-writer thread: the sole caller of
/// [`FinalizedState::commit_finalized_direct`].
pub(super) struct DiskWriter {
    /// The channel of writes and guard-control messages from the worker.
    pub(super) receiver: Receiver<DiskRequest>,

    /// The finalized state the blocks are committed to.
    pub(super) finalized_state: FinalizedState,

    /// The height most recently written to disk, published for the worker's
    /// prune loop.
    ///
    /// # Correctness
    ///
    /// This thread `Release`-stores a height **after** `commit_finalized_direct`
    /// returns; the worker `Acquire`-loads it before pruning. Reading height
    /// `H` in the worker therefore happens-after the completion of `H`'s
    /// database write, so a block is never pruned from the in-memory state
    /// before its on-disk copy is in place (the worker finds it in one of the
    /// two). The value is monotonic because, after this change, **all** stores
    /// happen on this single disk-writer thread — genesis and overflow roots
    /// included — in commit order, so concurrent updates can't be lost and
    /// there is no per-phase re-initialization. `Relaxed` would not document or
    /// guarantee the prune-after-write ordering.
    pub(super) disk_tip_height: Arc<AtomicU32>,
}

impl DiskWriter {
    /// Runs the disk-writer loop until the worker drops the request channel.
    ///
    /// On channel close the guard (if any) drops, which restores the
    /// write-ahead log, flushes WAL-skipped writes, and resumes
    /// auto-compaction; the enclosing thread scope's join propagates any panic
    /// to the worker exactly as the previous inline disk thread did.
    pub(super) fn run(mut self) {
        // The bulk-write guard, present only while checkpoint-stream blocks are
        // being written. Created lazily on the first bulk write, dropped on
        // EndBulk / a non-bulk write / channel close.
        let mut guard: Option<FinalizedWritePhase<crate::service::finalized_state::ZebraDb>> = None;

        // The note commitment trees threaded from each committed block to the
        // next, so checkpoint blocks don't re-read them from the database tip.
        let mut prev_note_commitment_trees: Option<NoteCommitmentTrees> = None;

        while let Ok(request) = self.receiver.recv() {
            match request {
                DiskRequest::Write { block, bulk, ack } => {
                    // The guard transition follows the data: enter bulk mode on
                    // the first bulk write, leave it on the first non-bulk
                    // write. Both are reversible and idempotent; at steady
                    // state both arms are no-ops.
                    Self::transition_guard(&mut guard, bulk, &self.finalized_state);

                    let height = block.height();
                    let result = self
                        .finalized_state
                        .commit_finalized_direct(
                            *block,
                            prev_note_commitment_trees.take(),
                            "checkpoint pipeline disk writer",
                        )
                        .map(|(hash, note_commitment_trees)| {
                            prev_note_commitment_trees = Some(note_commitment_trees);
                            hash
                        });

                    match result {
                        Ok(hash) => {
                            // Publish the on-disk tip height for the worker's
                            // prune loop. Release pairs with the worker's
                            // Acquire load so a block is never pruned before
                            // its write is visible.
                            self.disk_tip_height.store(height.0, Ordering::Release);

                            // Bound level 0 file growth while compaction is
                            // paused (only meaningful inside the bulk guard).
                            if let Some(guard) = guard.as_mut() {
                                guard.block_committed();
                            }

                            if bulk {
                                // Keep these metric names: the
                                // `create_cached_database` acceptance test's
                                // regex depends on them.
                                metrics::counter!("state.checkpoint.finalized.block.count")
                                    .increment(1);
                                metrics::gauge!("state.checkpoint.finalized.block.height")
                                    .set(height.0 as f64);
                                metrics::gauge!("zcash.chain.verified.block.height")
                                    .set(height.0 as f64);
                                metrics::counter!("zcash.chain.verified.block.total").increment(1);
                            }

                            if let Some(ack) = ack {
                                let _ = ack.send(Ok(hash));
                            }
                        }
                        Err(error) => {
                            if let Some(ack) = ack {
                                // The sender owns the recovery policy: report
                                // the error and keep running.
                                let _ = ack.send(Err(error));
                            } else {
                                // The block was acknowledged to its submitter
                                // after the in-memory commit and may have been
                                // observed via the chain tip. The ack can't be
                                // recalled, and memory and disk can no longer
                                // be reconciled. A crash here is recoverable
                                // (the write-ahead log, or the periodic atomic
                                // flush when the WAL is skipped); a silent
                                // divergence between memory and disk is not.
                                panic!(
                                    "unexpected disk write error for an already-acknowledged \
                                     block: {error:?}"
                                );
                            }
                        }
                    }
                }

                DiskRequest::EndBulk => {
                    // Dropping the guard flushes WAL-skipped writes and resumes
                    // auto-compaction. FIFO ordering means every write this
                    // guard covered has already been committed above.
                    if guard.take().is_some() {
                        Self::record_guard_transition();
                    }
                }
            }
        }

        // The channel closed: the guard drops here, restoring the WAL and
        // resuming compaction on this normal exit path.
    }

    /// Applies the guard transition implied by a write's `bulk` flag.
    ///
    /// Creates the guard on the first bulk write, drops it on the first
    /// non-bulk write. Records a transition metric on each create/drop so
    /// guard thrash is operator-visible.
    fn transition_guard(
        guard: &mut Option<FinalizedWritePhase<crate::service::finalized_state::ZebraDb>>,
        bulk: bool,
        finalized_state: &FinalizedState,
    ) {
        match (bulk, guard.is_some()) {
            (true, false) => {
                *guard = Some(FinalizedWritePhase::new(
                    finalized_state.db.clone(),
                    finalized_state.db.config().disable_wal_during_ibd,
                ));
                Self::record_guard_transition();
            }
            (false, true) => {
                *guard = None;
                Self::record_guard_transition();
            }
            // Already in the requested mode: no-op (the steady-state path).
            (true, true) | (false, false) => {}
        }
    }

    /// Counts a bulk-write-guard create or drop, so operators can see guard
    /// thrash (rapid bulk/non-bulk alternation).
    fn record_guard_transition() {
        metrics::counter!("state.checkpoint.bulk.transitions").increment(1);
    }
}

#[cfg(test)]
mod tests;
