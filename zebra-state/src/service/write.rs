//! Writing blocks to the finalized and non-finalized states.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
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
    transparent::{self, EXTRA_ZEBRA_COINBASE_DATA},
};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    request::{FinalizableBlock, FinalizedBlock},
    service::{
        check,
        finalized_state::{FinalizedState, OutputLocation, ZebraDb},
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
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

/// The capacity of the crossbeam channel from Thread 1 (block committer) to Thread 2
/// (UTXO/output-location lookup).
const PIPELINE_LOOKUP_CHANNEL_CAPACITY: usize = 100;

/// The capacity of the crossbeam channel from Thread 2 (UTXO lookup) to Thread 3
/// (batch preparation + disk writer).
const PIPELINE_WRITE_CHANNEL_CAPACITY: usize = 100;

/// Thread 2 → Thread 3: a finalized block with its pre-looked-up spent UTXOs.
type BlockWrite = (
    FinalizedBlock,
    Vec<(transparent::OutPoint, OutputLocation, transparent::Utxo)>,
);

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
        last_zebra_mined_log_height,
        backup_dir_path,
    ),
    fields(chains = non_finalized_state.chain_count())
)]
fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    last_zebra_mined_log_height: &mut Option<Height>,
    backup_dir_path: Option<&Path>,
) -> block::Height {
    let best_chain = non_finalized_state.best_chain().expect("unexpected empty non-finalized state: must commit at least one block before updating channels");

    let tip_block = best_chain
        .tip_block()
        .expect("unexpected empty chain: must commit at least one block before updating channels")
        .clone();
    let tip_block = ChainTipBlock::from(tip_block);

    log_if_mined_by_zebra(&tip_block, last_zebra_mined_log_height);

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

        let span = Span::current();
        let task = std::thread::spawn(move || {
            span.in_scope(|| {
                WriteBlockWorkerTask {
                    finalized_block_write_receiver,
                    non_finalized_block_write_receiver,
                    finalized_state,
                    non_finalized_state,
                    invalid_block_reset_sender,
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
                finalized: Some(finalized_block_write_sender)
                    .filter(|_| should_use_finalized_block_write_sender),
            },
            invalid_block_write_reset_receiver,
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
            chain_tip_sender,
            non_finalized_state_sender,
            backup_dir_path,
        } = &mut self;

        let mut last_zebra_mined_log_height = None;
        let mut prev_finalized_note_commitment_trees = None;

        // Disable auto-compaction during checkpoint sync for faster writes.
        // WAL stays enabled for crash safety during network sync.
        finalized_state
            .db
            .set_auto_compaction(false);

        // Pipeline: commit checkpoint-verified blocks to the non-finalized state (Thread 1),
        // look up spent UTXOs/output-locations (Thread 2), then prepare batch and write to
        // disk (Thread 3). This avoids blocking Thread 1 on any disk I/O.
        let (lookup_tx, lookup_rx) =
            crossbeam_channel::bounded::<FinalizedBlock>(PIPELINE_LOOKUP_CHANNEL_CAPACITY);
        let (write_tx, write_rx) =
            crossbeam_channel::bounded::<BlockWrite>(PIPELINE_WRITE_CHANNEL_CAPACITY);

        // Subscribe to the non-finalized state watch channel before spawning Thread 2,
        // so the receiver is moved into the thread.
        let nfs_receiver = non_finalized_state_sender.subscribe();

        std::thread::scope(|s| {
            // Thread 2: look up spent UTXOs and output locations
            let db2 = finalized_state.db.clone();
            s.spawn(move || {
                let mut t2_sum: u128 = 0;
                let mut t2_max: u128 = 0;
                let mut t2_count: u64 = 0;

                while let Ok(msg) = lookup_rx.recv() {
                    let t = std::time::Instant::now();
                    // Clone the latest non-finalized state once per block so that UTXOs
                    // created by blocks not yet on disk can be found.
                    let latest_nfs = nfs_receiver.borrow().clone();
                    let spent_utxos = db2.lookup_spent_utxos(&msg, &latest_nfs);
                    let lookup_us = t.elapsed().as_micros();

                    t2_sum += lookup_us;
                    if lookup_us > t2_max { t2_max = lookup_us; }
                    t2_count += 1;

                    if t2_count % 100 == 0 {
                        tracing::info!(
                            height = ?msg.height,
                            avg_lookup_us = t2_sum / u128::from(t2_count),
                            max_lookup_us = t2_max,
                            blocks = t2_count,
                            "pipeline_thread2_summary",
                        );
                    }
                    write_tx
                        .send((msg, spent_utxos))
                        .expect("disk writer thread should be alive");
                }
            });

            // Thread 3: prepare batch and write to disk
            let mut finalized_state3 = finalized_state.clone();
            s.spawn(move || {
                let mut prev_note_commitment_trees: Option<NoteCommitmentTrees> = None;
                let mut t3_sum: u128 = 0;
                let mut t3_max: u128 = 0;
                let mut t3_count: u64 = 0;

                while let Ok((finalized, spent_utxos)) = write_rx.recv() {
                    let note_commitment_trees = finalized.treestate.note_commitment_trees.clone();
                    let height = finalized.height;

                    let t = std::time::Instant::now();
                    finalized_state3
                        .commit_finalized_direct_internal(
                            finalized,
                            prev_note_commitment_trees.take(),
                            Some(spent_utxos),
                            "commit checkpoint-verified pipeline",
                        )
                        .expect(
                            "unexpected disk write error: \
                         block has already been validated by the non-finalized state",
                        );
                    let commit_us = t.elapsed().as_micros();

                    t3_sum += commit_us;
                    if commit_us > t3_max { t3_max = commit_us; }
                    t3_count += 1;

                    if t3_count % 100 == 0 {
                        tracing::info!(
                            ?height,
                            avg_commit_us = t3_sum / u128::from(t3_count),
                            max_commit_us = t3_max,
                            blocks = t3_count,
                            "pipeline_thread3_summary",
                        );
                    }

                    prev_note_commitment_trees = Some(note_commitment_trees);

                    metrics::counter!("state.checkpoint.finalized.block.count").increment(1);
                    metrics::gauge!("state.checkpoint.finalized.block.height").set(height.0 as f64);
                    metrics::gauge!("zcash.chain.verified.block.height").set(height.0 as f64);
                    metrics::counter!("zcash.chain.verified.block.total").increment(1);
                }
            });

            // Thread 1: commit checkpoint-verified blocks to the non-finalized state
            // and feed them into the pipeline.
            let mut next_expected_height = finalized_state
                .db
                .finalized_tip_height()
                .map(|height| (height + 1).expect("committed heights are valid"))
                .unwrap_or(Height(0));

            // Thread 1 accumulators for periodic summary logging
            let mut t1_nfs_sum: u128 = 0;
            let mut t1_nfs_max: u128 = 0;
            let mut t1_send_sum: u128 = 0;
            let mut t1_send_max: u128 = 0;
            let mut t1_peek_sum: u128 = 0;
            let mut t1_peek_max: u128 = 0;
            let mut t1_count: u64 = 0;

            while let Some((checkpoint_verified, rsp_tx)) =
                finalized_block_write_receiver.blocking_recv()
            {
                if invalid_block_reset_sender.is_closed() {
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    return;
                }

                // Discard blocks at the wrong height (e.g. descendants of a failed block).
                if checkpoint_verified.height != next_expected_height {
                    debug!(
                        ?next_expected_height,
                        invalid_height = ?checkpoint_verified.height,
                        invalid_hash = ?checkpoint_verified.hash,
                        "got a block that was the wrong height. \
                         Assuming a parent block failed, and dropping this block",
                    );
                    std::mem::drop((checkpoint_verified, rsp_tx));
                    continue;
                }

                // The genesis block must be committed directly to disk because
                // the non-finalized state's Chain requires a finalized tip to
                // initialize its tree state from.
                if checkpoint_verified.height == Height(0) {
                    match finalized_state.commit_finalized(
                        (checkpoint_verified, rsp_tx),
                        prev_finalized_note_commitment_trees.take(),
                    ) {
                        Ok((committed, note_commitment_trees)) => {
                            let tip_block = ChainTipBlock::from(committed);
                            prev_finalized_note_commitment_trees = Some(note_commitment_trees);

                            log_if_mined_by_zebra(&tip_block, &mut last_zebra_mined_log_height);
                            chain_tip_sender.set_finalized_tip(tip_block);
                        }
                        Err(error) => {
                            let finalized_tip = finalized_state.db.tip();
                            info!(
                                ?error,
                                last_valid_height = ?finalized_tip.map(|tip| tip.0),
                                last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                                "committing genesis block to the finalized state failed",
                            );

                            let send_result = invalid_block_reset_sender
                                .send(finalized_state.db.finalized_tip_hash());
                            if send_result.is_err() {
                                info!(
                                    "StateService closed the block reset channel. \
                                       Is Zebra shutting down?"
                                );
                                return;
                            }
                        }
                    }

                    next_expected_height = (next_expected_height + 1)
                        .expect("block heights in the pipeline are valid");
                    continue;
                }

                // Commit block to the non-finalized state (fast, in-memory).
                let t_nfs = std::time::Instant::now();
                let tip_block = non_finalized_state
                    .commit_checkpoint_block(checkpoint_verified.clone(), &finalized_state.db)
                    .expect(
                        "checkpoint block commit to non-finalized state should succeed \
                         because the checkpoint verifier has already validated the hash chain",
                    );
                let nfs_commit_us = t_nfs.elapsed().as_micros();

                // Send the updated non-finalized state to the watch channel BEFORE sending
                // the block to Thread 2, so Thread 2 is guaranteed to see the latest UTXOs.
                let t_send = std::time::Instant::now();
                let _ = non_finalized_state_sender.send(non_finalized_state.clone());
                let nfs_send_us = t_send.elapsed().as_micros();

                log_if_mined_by_zebra(&tip_block, &mut last_zebra_mined_log_height);
                chain_tip_sender.set_best_non_finalized_tip(tip_block);

                // Get the finalizable block with its treestate already computed.
                let t_peek = std::time::Instant::now();
                let finalizable = non_finalized_state
                    .peek_finalize_tip()
                    .expect("just committed a block to the non-finalized state");

                let finalized_block = match finalizable {
                    FinalizableBlock::Contextual {
                        contextually_verified,
                        treestate,
                    } => {
                        FinalizedBlock::from_contextually_verified(contextually_verified, treestate)
                    }
                    FinalizableBlock::Checkpoint { .. } => {
                        unreachable!("peek_finalize_tip always returns Contextual")
                    }
                };
                let peek_finalize_us = t_peek.elapsed().as_micros();

                // Send to the pipeline for UTXO lookup and disk write.
                lookup_tx
                    .send(finalized_block)
                    .expect("UTXO lookup thread should be alive");

                // Respond to the caller immediately — the block is in the non-finalized
                // state and queryable by the state service.
                let _ = rsp_tx.send(Ok(checkpoint_verified.hash));

                t1_nfs_sum += nfs_commit_us;
                if nfs_commit_us > t1_nfs_max { t1_nfs_max = nfs_commit_us; }
                t1_send_sum += nfs_send_us;
                if nfs_send_us > t1_send_max { t1_send_max = nfs_send_us; }
                t1_peek_sum += peek_finalize_us;
                if peek_finalize_us > t1_peek_max { t1_peek_max = peek_finalize_us; }
                t1_count += 1;

                if t1_count % 100 == 0 {
                    let n = u128::from(t1_count);
                    tracing::info!(
                        height = ?checkpoint_verified.height,
                        avg_nfs_commit_us = t1_nfs_sum / n,
                        max_nfs_commit_us = t1_nfs_max,
                        avg_nfs_send_us = t1_send_sum / n,
                        max_nfs_send_us = t1_send_max,
                        avg_peek_finalize_us = t1_peek_sum / n,
                        max_peek_finalize_us = t1_peek_max,
                        blocks = t1_count,
                        "pipeline_thread1_summary",
                    );
                }

                // Prune blocks from the non-finalized state that have already
                // been written to disk by Thread 3.
                if let Some(finalized_tip_height) = finalized_state.db.finalized_tip_height() {
                    while non_finalized_state
                        .root_height()
                        .is_some_and(|root| root <= finalized_tip_height)
                    {
                        non_finalized_state.finalize();
                    }
                }

                next_expected_height =
                    (next_expected_height + 1).expect("block heights in the pipeline are valid");
            }

            // Drop the sender to signal the pipeline to drain and finish.
            drop(lookup_tx);
            // Scoped threads are automatically joined when the scope exits.
        });

        // Checkpoint sync is complete. Re-enable auto-compaction
        // for normal operation during full verification.
        finalized_state
            .db
            .set_auto_compaction(true);

        // All checkpoint blocks have been written to disk by the pipeline.
        // Remove them from the non-finalized state so Phase 2 starts clean.
        while non_finalized_state.best_chain_len().unwrap_or(0) > 0 {
            non_finalized_state.finalize();
        }

        // Update channels to reflect the now-empty non-finalized state.
        let _ = non_finalized_state_sender.send(non_finalized_state.clone());

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
                    &mut last_zebra_mined_log_height,
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
                &mut last_zebra_mined_log_height,
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

/// Log a message if this block was mined by Zebra.
///
/// Does not detect early Zebra blocks, and blocks with custom coinbase transactions.
/// Rate-limited to every 1000 blocks using `last_zebra_mined_log_height`.
fn log_if_mined_by_zebra(
    tip_block: &ChainTipBlock,
    last_zebra_mined_log_height: &mut Option<Height>,
) {
    // This logs at most every 2-3 checkpoints, which seems fine.
    const LOG_RATE_LIMIT: u32 = 1000;

    let height = tip_block.height.0;

    if let Some(last_height) = last_zebra_mined_log_height {
        if height < last_height.0 + LOG_RATE_LIMIT {
            // If we logged in the last 1000 blocks, don't log anything now.
            return;
        }
    };

    // This code is rate-limited, so we can do expensive transformations here.
    let coinbase_data = tip_block.transactions[0].inputs()[0]
        .extra_coinbase_data()
        .expect("valid blocks must start with a coinbase input")
        .clone();

    if coinbase_data
        .as_ref()
        .starts_with(EXTRA_ZEBRA_COINBASE_DATA.as_bytes())
    {
        let text = String::from_utf8_lossy(coinbase_data.as_ref());

        *last_zebra_mined_log_height = Some(Height(height));

        // No need for hex-encoded data if it's exactly what we expected.
        if coinbase_data.as_ref() == EXTRA_ZEBRA_COINBASE_DATA.as_bytes() {
            info!(
                %text,
                %height,
                hash = %tip_block.hash,
                "looks like this block was mined by Zebra!"
            );
        } else {
            // # Security
            //
            // Use the extra data as an allow-list, replacing unknown characters.
            // This makes sure control characters and harmful messages don't get logged
            // to the terminal.
            let text = text.replace(
                |c: char| {
                    !EXTRA_ZEBRA_COINBASE_DATA
                        .to_ascii_lowercase()
                        .contains(c.to_ascii_lowercase())
                },
                "?",
            );
            let data = hex::encode(coinbase_data.as_ref());

            info!(
                %text,
                %data,
                %height,
                hash = %tip_block.hash,
                "looks like this block was mined by Zebra!"
            );
        }
    }
}
