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
    transparent::EXTRA_ZEBRA_COINBASE_DATA,
};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{
        check,
        finalized_state::{FinalizedState, ZebraDb},
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

        // Pipelined checkpoint sync: three-thread architecture.
        //
        // Thread 1 (this thread): Receives checkpoint blocks, commits them to
        // the in-memory NonFinalizedState (fast), and sends FinalizableBlocks
        // along with a snapshot of the non-finalized state to Thread 2.
        //
        // Thread 2 (pre-fetch, spawned below): Receives FinalizableBlocks,
        // converts them to FinalizedBlocks (computing treestates), and
        // pre-fetches spent UTXO data using batched multi_get_cf. Sends the
        // block + pre-fetched data to Thread 3.
        //
        // Thread 3 (disk writer, spawned below): Receives FinalizedBlocks with
        // pre-fetched UTXO data and writes them to RocksDB without needing
        // per-input database lookups.

        // Channel from Thread 1 → Thread 2: carries blocks + non-finalized state.
        let (finalize_tx, mut finalize_rx) = tokio::sync::mpsc::unbounded_channel::<(
            crate::request::FinalizableBlock,
            NonFinalizedState,
        )>();

        // Channel from Thread 2 → Thread 3: carries finalized blocks + prefetched data.
        type PrefetchedMessage = (
            crate::request::FinalizableBlock,
            crate::service::finalized_state::PrefetchedBlockWriteData,
        );
        let (prefetched_tx, prefetched_rx) = std::sync::mpsc::channel::<PrefetchedMessage>();

        // The disk writer reports its committed height so we know when it's
        // safe to prune blocks from the non-finalized state.
        let (committed_height_tx, committed_height_rx) =
            tokio::sync::watch::channel::<Height>(Height(0));

        // Thread 2: Pre-fetch spent UTXOs using batched multi_get_cf.
        let prefetch_db = finalized_state.db.clone();
        let prefetch_span = tracing::info_span!("finalized_utxo_prefetch");
        let prefetch_thread = std::thread::spawn(move || {
            let _guard = prefetch_span.enter();

            while let Some((finalizable, nfs_snapshot)) = finalize_rx.blocking_recv() {
                let (block, height, new_outputs) = match &finalizable {
                    crate::request::FinalizableBlock::Checkpoint {
                        checkpoint_verified,
                    } => (
                        checkpoint_verified.block.clone(),
                        checkpoint_verified.height,
                        checkpoint_verified.new_outputs.clone(),
                    ),
                    crate::request::FinalizableBlock::Contextual {
                        contextually_verified,
                        ..
                    } => (
                        contextually_verified.block.clone(),
                        contextually_verified.height,
                        contextually_verified.new_outputs.clone(),
                    ),
                };

                // Get unspent UTXOs from the non-finalized state (in-memory).
                let nfs_utxos = nfs_snapshot
                    .best_chain()
                    .map(|chain| chain.unspent_utxos())
                    .unwrap_or_default();

                let prefetched =
                    prefetch_db.fetch_block_write_data(&block, height, &new_outputs, &nfs_utxos);

                if prefetched_tx.send((finalizable, prefetched)).is_err() {
                    info!("disk writer channel closed, stopping prefetch");
                    break;
                }
            }
        });

        // Thread 3: Disk writer — receives pre-fetched blocks and writes to DB.
        let mut disk_finalized_state = finalized_state.clone();
        let disk_span = tracing::info_span!("finalized_disk_writer");
        let disk_thread = std::thread::spawn(move || {
            let _guard = disk_span.enter();
            let mut prev_trees = None;
            let mut prev_history_tree = None;

            while let Ok((finalizable, prefetched)) = prefetched_rx.recv() {
                let result = disk_finalized_state.commit_finalized_direct_with_trees(
                    finalizable,
                    prev_trees.take(),
                    prev_history_tree.take(),
                    "commit checkpoint-verified request",
                    Some(prefetched),
                );

                match result {
                    Ok((_hash, note_commitment_trees, history_tree)) => {
                        let height = disk_finalized_state
                            .db
                            .finalized_tip_height()
                            .expect("just committed a block");
                        prev_trees = Some(note_commitment_trees);
                        prev_history_tree = Some(history_tree);
                        let _ = committed_height_tx.send(height);
                    }
                    Err(error) => {
                        info!(?error, "disk finalization failed");
                    }
                }
            }
        });

        // Track the next expected height in memory so we don't read stale
        // values from the DB (which lags behind during pipelined sync).
        let mut next_valid_height = finalized_state
            .db
            .finalized_tip_height()
            .map(|height| (height + 1).expect("committed heights are valid"))
            .unwrap_or(Height(0));

        while let Some(ordered_block) = finalized_block_write_receiver.blocking_recv() {
            if invalid_block_reset_sender.is_closed() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                break;
            }

            if ordered_block.0.height != next_valid_height {
                debug!(
                    ?next_valid_height,
                    invalid_height = ?ordered_block.0.height,
                    invalid_hash = ?ordered_block.0.hash,
                    "got a block that was the wrong height. \
                     Assuming a parent block failed, and dropping this block",
                );
                std::mem::drop(ordered_block);
                continue;
            }

            let (checkpoint_verified, rsp_tx) = ordered_block;

            // For genesis (height 0), commit directly to the finalized state
            // since the non-finalized chain needs an existing finalized tip.
            if checkpoint_verified.height == Height(0) {
                match finalized_state.commit_finalized(
                    (checkpoint_verified.clone(), rsp_tx),
                    prev_finalized_note_commitment_trees.take(),
                ) {
                    Ok((finalized, note_commitment_trees)) => {
                        let tip_block = ChainTipBlock::from(finalized);
                        prev_finalized_note_commitment_trees = Some(note_commitment_trees);
                        log_if_mined_by_zebra(&tip_block, &mut last_zebra_mined_log_height);
                        chain_tip_sender.set_finalized_tip(tip_block);
                        next_valid_height = (next_valid_height + 1).expect("valid height");
                    }
                    Err(error) => {
                        info!(?error, "genesis block commit failed");
                        let _ = invalid_block_reset_sender
                            .send(finalized_state.db.finalized_tip_hash());
                    }
                }
                continue;
            }

            // Fast path: commit to in-memory NonFinalizedState.
            match non_finalized_state
                .commit_checkpoint_block(checkpoint_verified.clone(), &finalized_state.db)
            {
                Ok(tip_block) => {
                    log_if_mined_by_zebra(&tip_block, &mut last_zebra_mined_log_height);
                    chain_tip_sender.set_finalized_tip(tip_block);

                    next_valid_height =
                        (next_valid_height + 1).expect("committed heights are valid");

                    // Send the response immediately — the block is now in the
                    // non-finalized state and queryable by the read service.
                    let _ = rsp_tx.send(Ok(checkpoint_verified.hash));

                    metrics::counter!("state.checkpoint.finalized.block.count").increment(1);
                    metrics::gauge!("state.checkpoint.finalized.block.height")
                        .set(checkpoint_verified.height.0 as f64);
                    metrics::gauge!("zcash.chain.verified.block.height")
                        .set(checkpoint_verified.height.0 as f64);
                    metrics::counter!("zcash.chain.verified.block.total").increment(1);

                    // Publish the non-finalized state so the read service
                    // sees both new blocks and pruned committed blocks.
                    let _ = non_finalized_state_sender.send(non_finalized_state.clone());

                    // Prune blocks the disk writer has already committed
                    // (they're safely on disk and no longer needed in memory).
                    let disk_tip_height = *committed_height_rx.borrow();
                    while non_finalized_state
                        .root_height()
                        .is_some_and(|h| h <= disk_tip_height)
                    {
                        non_finalized_state.finalize();
                    }

                    // Send this block to the disk writer immediately
                    // using the contextually verified data and treestate
                    // already computed during the chain update. The block
                    // stays in the non-finalized state so it remains
                    // queryable until the disk writer confirms the commit
                    // and the prune loop above removes it.
                    //
                    // The unbounded channel ensures this never blocks the
                    // commit path; backpressure comes from the prune loop
                    // waiting on committed_height_rx.
                    if let Some(finalizable) = non_finalized_state.peek_finalize_tip() {
                        if finalize_tx
                            .send((finalizable, non_finalized_state.clone()))
                            .is_err()
                        {
                            warn!("disk writer channel closed unexpectedly");
                        }
                    }
                }
                Err(error) => {
                    info!(
                        ?error,
                        "checkpoint block commit to non-finalized state failed"
                    );
                    let _ = rsp_tx.send(Err(error.into()));

                    let send_result =
                        invalid_block_reset_sender.send(finalized_state.db.finalized_tip_hash());
                    if send_result.is_err() {
                        info!(
                            "StateService closed the block reset channel. Is Zebra shutting down?"
                        );
                        break;
                    }
                }
            }
        }

        // All blocks were already sent to the disk writer when they were
        // committed to the non-finalized state. Drain the non-finalized
        // chain so it's empty before the non-finalized block phase begins.
        while non_finalized_state.best_chain_len().unwrap_or(0) > 0 {
            non_finalized_state.finalize();
        }

        // Drop the channel sender to signal the prefetch and disk threads
        // to finish, then wait for them to complete all remaining blocks.
        // Dropping finalize_tx → prefetch thread exits → drops prefetched_tx → disk thread exits.
        drop(finalize_tx);
        if let Err(panic) = prefetch_thread.join() {
            std::panic::resume_unwind(panic);
        }
        if let Err(panic) = disk_thread.join() {
            std::panic::resume_unwind(panic);
        }

        // Publish the now-empty non-finalized state so the read service
        // stops seeing blocks that have been moved to the finalized DB.
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
