//! Writing blocks to the finalized and non-finalized states.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use zebra_chain::{
    block::{self, Height},
    parameters::Network,
};

use crate::{
    service::{
        check,
        finalized_state::FinalizedState,
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedFinalized, QueuedNonFinalized},
        BoxError, ChainTipBlock, ChainTipSender, CloneError,
    },
    CommitBlockError, PreparedBlock,
};

use std::collections::HashMap;

pub enum NonFinalizedWriteCmd {
    ProcessQueued {
        parent_hash: block::Hash,
        queued_blocks: Vec<QueuedNonFinalized>,
    },
    FinishedProcessQueued,
}

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Run contextual validation on the prepared block and add it to the
/// non-finalized state if it is contextually valid.
#[tracing::instrument(level = "debug", skip(prepared))]
pub(crate) fn validate_and_commit_non_finalized(
    finalized_state: &mut FinalizedState,
    non_finalized_state: &mut NonFinalizedState,
    network: Network,
    prepared: PreparedBlock,
) -> Result<(), CommitBlockError> {
    check::contextual_validity(finalized_state, non_finalized_state, network, &prepared)?;
    let parent_hash = prepared.block.header.previous_block_hash;

    if finalized_state.db.finalized_tip_hash() == parent_hash {
        non_finalized_state.commit_new_chain(prepared, finalized_state)?;
    } else {
        non_finalized_state.commit_block(prepared, finalized_state)?;
    }

    Ok(())
}

/// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
/// channels with the latest non-finalized [`ChainTipBlock`] and
/// [`Chain`][1].
///
/// Returns the latest non-finalized chain tip height, or `None` if the
/// non-finalized state is empty.
///
/// [1]: non_finalized_state::Chain
//
// TODO: remove this clippy allow when we remove self.chain_tip_sender
#[allow(clippy::unwrap_in_result)]
#[instrument(level = "debug", skip(chain_tip_sender))]
fn update_latest_chain_channels(
    finalized_state: &FinalizedState,
    non_finalized_state: &NonFinalizedState,
    chain_tip_sender: Arc<Mutex<ChainTipSender>>,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
) -> Option<block::Height> {
    let best_chain = non_finalized_state.best_chain();
    let tip_block = best_chain
        .and_then(|chain| chain.tip_block())
        .cloned()
        .map(ChainTipBlock::from);
    let tip_block_height = tip_block.as_ref().map(|block| block.height);

    // TODO: Update the non_finalized_state_receiver
    // If the final receiver was just dropped, ignore the error.
    let _ = non_finalized_state_sender.send(non_finalized_state.clone());

    chain_tip_sender
        .lock()
        .expect("unexpected panic in block commit task or state")
        .set_best_non_finalized_tip(tip_block);

    tip_block_height
}

/// Reads blocks from the channels, write them to the `finalized_state`,
/// and updates the
#[instrument(skip(
    finalized_block_write_receiver,
    non_finalized_block_write_receiver,
    invalid_block_reset_sender,
    chain_tip_sender
))]
pub fn write_blocks_from_channels(
    mut finalized_block_write_receiver: UnboundedReceiver<QueuedFinalized>,
    mut non_finalized_block_write_receiver: UnboundedReceiver<NonFinalizedWriteCmd>,
    mut finalized_state: FinalizedState,
    mut non_finalized_state: NonFinalizedState,
    network: Network,
    invalid_block_reset_sender: UnboundedSender<block::Hash>,
    chain_tip_sender: Arc<Mutex<ChainTipSender>>,
    non_finalized_state_sender: watch::Sender<NonFinalizedState>,
) {
    while let Some(ordered_block) = finalized_block_write_receiver.blocking_recv() {
        // TODO: split these checks into separate functions

        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // Discard any children of invalid blocks in the channel
        let next_valid_height = finalized_state
            .db
            .finalized_tip_height()
            .map(|height| (height + 1).expect("committed heights are valid"))
            .unwrap_or(Height(0));

        if ordered_block.0.height > next_valid_height {
            debug!(
                ?next_valid_height,
                invalid_height = ?ordered_block.0.height,
                invalid_hash = ?ordered_block.0.hash,
                "got a block that was too high. \
                 Assuming a parent block failed, and dropping this block",
            );

            // We don't want to send a reset here, because it could overwrite a valid sent hash
        }

        // Try committing the block
        match finalized_state.commit_finalized(ordered_block) {
            Ok(finalized) => {
                let tip_block = ChainTipBlock::from(finalized);

                // TODO: update the chain tip sender with non-finalized blocks in this function,
                //       and get rid of the mutex
                chain_tip_sender
                    .lock()
                    .expect("unexpected panic in block commit task or state")
                    .set_finalized_tip(tip_block);
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
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    return;
                }
            }
        }
    }

    // We're finished receiving finalized blocks from the state.
    finalized_block_write_receiver.close();
    std::mem::drop(finalized_block_write_receiver);

    if invalid_block_reset_sender.is_closed() {
        info!("StateService closed the block reset channel. Is Zebra shutting down?");
        return;
    }

    let mut parent_error_map: HashMap<block::Hash, CloneError> = HashMap::new();

    while let Some(non_finalized_write_cmd) = non_finalized_block_write_receiver.blocking_recv() {
        match non_finalized_write_cmd {
            NonFinalizedWriteCmd::ProcessQueued {
                parent_hash,
                queued_blocks,
            } => {
                let parent_error = parent_error_map.remove(&parent_hash);

                for (child, rsp_tx) in queued_blocks {
                    let child_hash = child.hash;
                    let result;

                    // If the block is invalid, reject any descendant blocks.
                    //
                    // At this point, we know that the block and all its descendants
                    // are invalid, because we checked all the consensus rules before
                    // committing the block to the non-finalized state.
                    // (These checks also bind the transaction data to the block
                    // header, using the transaction merkle tree and authorizing data
                    // commitment.)
                    if let Some(ref parent_error) = parent_error {
                        tracing::trace!(
                            ?child_hash,
                            ?parent_error,
                            "rejecting queued child due to parent error"
                        );
                        result = Err(parent_error.clone());
                    } else {
                        tracing::trace!(?child_hash, "validating queued child");
                        result = validate_and_commit_non_finalized(
                            &mut finalized_state,
                            &mut non_finalized_state,
                            network,
                            child,
                        )
                        .map_err(CloneError::from);

                        if result.is_ok() {
                            // Update the metrics if semantic and contextual validation passes
                            metrics::counter!("state.full_verifier.committed.block.count", 1);
                            metrics::counter!("zcash.chain.verified.block.total", 1);
                        }
                    }

                    let _ =
                        rsp_tx.send(result.clone().map(|()| child_hash).map_err(BoxError::from));

                    if let Err(ref error) = result {
                        parent_error_map.insert(parent_hash, error.clone());
                    }
                }
            }

            NonFinalizedWriteCmd::FinishedProcessQueued => {
                // Clear out any errors from processed queued blocks without children
                parent_error_map.clear();

                while non_finalized_state.best_chain_len()
                    > crate::constants::MAX_BLOCK_REORG_HEIGHT
                {
                    tracing::trace!("finalizing block past the reorg limit");
                    let finalized_with_trees = non_finalized_state.finalize();
                    finalized_state
                        .commit_finalized_direct(finalized_with_trees, "best non-finalized chain root")
                        .expect(
                            "expected that errors would not occur when writing to disk or updating note commitment and history trees",
                        );
                }

                let tip_block_height = update_latest_chain_channels(
                    &finalized_state,
                    &non_finalized_state,
                    Arc::clone(&chain_tip_sender),
                    &non_finalized_state_sender,
                );

                // update metrics using the best non-finalized tip
                if let Some(tip_block_height) = tip_block_height {
                    metrics::gauge!(
                        "state.full_verifier.committed.block.height",
                        tip_block_height.0 as f64,
                    );

                    // This height gauge is updated for both fully verified and checkpoint blocks.
                    // These updates can't conflict, because the state makes sure that blocks
                    // are committed in order.
                    metrics::gauge!(
                        "zcash.chain.verified.block.height",
                        tip_block_height.0 as f64,
                    );
                }

                tracing::trace!("finished processing queued block");
            }
        }

        // TODO:
        // - handle errors by sending a reset with all the block hashes in the non-finalized state, and the finalized tip
        // - update the chain tip sender and cached non-finalized state
    }

    // We're finished receiving non-finalized blocks from the state.
    //
    // TODO:
    // - make the task an object, and do this in the drop impl?
    // - does the drop order matter here?
    non_finalized_block_write_receiver.close();
    std::mem::drop(non_finalized_block_write_receiver);

    // We're done writing to the finalized state, so we can force it to shut down.
    finalized_state.db.shutdown(true);
    std::mem::drop(finalized_state);
}
