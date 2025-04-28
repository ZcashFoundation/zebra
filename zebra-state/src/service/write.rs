//! Writing blocks to the finalized and non-finalized states.

use std::sync::Arc;

use indexmap::IndexMap;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    watch,
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
        BoxError, ChainTipBlock, ChainTipSender, CloneError,
    },
    CommitSemanticallyVerifiedError, SemanticallyVerifiedBlock,
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
) -> Result<(), CommitSemanticallyVerifiedError> {
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
        last_zebra_mined_log_height
    ),
    fields(chains = non_finalized_state.chain_count())
)]
fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    last_zebra_mined_log_height: &mut Option<Height>,
) -> block::Height {
    let best_chain = non_finalized_state.best_chain().expect("unexpected empty non-finalized state: must commit at least one block before updating channels");

    let tip_block = best_chain
        .tip_block()
        .expect("unexpected empty chain: must commit at least one block before updating channels")
        .clone();
    let tip_block = ChainTipBlock::from(tip_block);

    log_if_mined_by_zebra(&tip_block, last_zebra_mined_log_height);

    let tip_block_height = tip_block.height;

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
}

/// The message type for the non-finalized block write task channel.
pub enum NonFinalizedWriteMessage {
    /// A newly downloaded and semantically verified block prepared for
    /// contextual validation and insertion into the non-finalized state.
    Commit(QueuedSemanticallyVerified),
    /// The hash of a block that should be invalidated and removed from
    /// the non-finalized state, if present.
    Invalidate(block::Hash),
    /// The hash of a block that was previously invalidated but should be
    /// reconsidered and reinserted into the non-finalized state.
    Reconsider(block::Hash),
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
                }
                .run()
            })
        });

        (
            Self {
                non_finalized: Some(non_finalized_block_write_sender),
                finalized: Some(finalized_block_write_sender),
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
        } = &mut self;

        let mut last_zebra_mined_log_height = None;
        let mut prev_finalized_note_commitment_trees = None;

        // Write all the finalized blocks sent by the state,
        // until the state closes the finalized block channel's sender.
        while let Some(ordered_block) = finalized_block_write_receiver.blocking_recv() {
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
            let next_valid_height = finalized_state
                .db
                .finalized_tip_height()
                .map(|height| (height + 1).expect("committed heights are valid"))
                .unwrap_or(Height(0));

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

            // Try committing the block
            match finalized_state
                .commit_finalized(ordered_block, prev_finalized_note_commitment_trees.take())
            {
                Ok((finalized, note_commitment_trees)) => {
                    let tip_block = ChainTipBlock::from(finalized);
                    prev_finalized_note_commitment_trees = Some(note_commitment_trees);

                    log_if_mined_by_zebra(&tip_block, &mut last_zebra_mined_log_height);

                    chain_tip_sender.set_finalized_tip(tip_block);
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

                    let send_result =
                        invalid_block_reset_sender.send(finalized_state.db.finalized_tip_hash());

                    if send_result.is_err() {
                        info!(
                            "StateService closed the block reset channel. Is Zebra shutting down?"
                        );
                        return;
                    }
                }
            }
        }

        // Do this check even if the channel got closed before any finalized blocks were sent.
        // This can happen if we're past the finalized tip.
        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // Save any errors to propagate down to queued child blocks
        let mut parent_error_map: IndexMap<block::Hash, CloneError> = IndexMap::new();

        while let Some(msg) = non_finalized_block_write_receiver.blocking_recv() {
            let queued_child_and_rsp_tx = match msg {
                NonFinalizedWriteMessage::Commit(queued_child) => Some(queued_child),
                NonFinalizedWriteMessage::Invalidate(hash) => {
                    non_finalized_state.invalidate_block(hash);
                    None
                }
                NonFinalizedWriteMessage::Reconsider(_hash) => {
                    // TODO: Uncomment after #9260 has merged.
                    // non_finalized_state.reconsider_block(hash);
                    None
                }
            };

            let Some((queued_child, rsp_tx)) = queued_child_and_rsp_tx else {
                update_latest_chain_channels(
                    non_finalized_state,
                    chain_tip_sender,
                    non_finalized_state_sender,
                    &mut last_zebra_mined_log_height,
                );
                continue;
            };

            let child_hash = queued_child.hash;
            let parent_hash = queued_child.block.header.previous_block_hash;
            let parent_error = parent_error_map.get(&parent_hash);

            let result;

            // If the parent block was marked as rejected, also reject all its children.
            //
            // At this point, we know that all the block's descendants
            // are invalid, because we checked all the consensus rules before
            // committing the failing ancestor block to the non-finalized state.
            if let Some(parent_error) = parent_error {
                tracing::trace!(
                    ?child_hash,
                    ?parent_error,
                    "rejecting queued child due to parent error"
                );
                result = Err(parent_error.clone());
            } else {
                tracing::trace!(?child_hash, "validating queued child");
                result = validate_and_commit_non_finalized(
                    &finalized_state.db,
                    non_finalized_state,
                    queued_child,
                )
                .map_err(CloneError::from);
            }

            // TODO: fix the test timing bugs that require the result to be sent
            //       after `update_latest_chain_channels()`,
            //       and send the result on rsp_tx here

            if let Err(ref error) = result {
                // Update the caller with the error.
                let _ = rsp_tx.send(result.clone().map(|()| child_hash).map_err(BoxError::from));

                // If the block is invalid, mark any descendant blocks as rejected.
                parent_error_map.insert(child_hash, error.clone());

                // Make sure the error map doesn't get too big.
                if parent_error_map.len() > PARENT_ERROR_MAP_LIMIT {
                    // We only add one hash at a time, so we only need to remove one extra here.
                    parent_error_map.shift_remove_index(0);
                }

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
            );

            // Update the caller with the result.
            let _ = rsp_tx.send(result.clone().map(|()| child_hash).map_err(BoxError::from));

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
