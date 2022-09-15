//! Writing blocks to the finalized and non-finalized states.

use std::sync::{Arc, Mutex};

use zebra_chain::block::{self, Height};

use crate::service::{
    finalized_state::FinalizedState,
    queued_blocks::{QueuedFinalized, QueuedNonFinalized},
    ChainTipBlock, ChainTipSender,
};

/// Reads blocks from the channels, write them to the `finalized_state`,
/// and updates the
///
/// TODO: pass the non-finalized state and associated update channel to this function
#[instrument(skip(
    finalized_block_write_receiver,
    non_finalized_block_write_receiver,
    invalid_block_reset_sender,
    chain_tip_sender
))]
pub fn write_blocks_from_channels(
    mut finalized_block_write_receiver: tokio::sync::mpsc::UnboundedReceiver<QueuedFinalized>,
    mut non_finalized_block_write_receiver: tokio::sync::mpsc::UnboundedReceiver<
        QueuedNonFinalized,
    >,
    mut finalized_state: FinalizedState,
    invalid_block_reset_sender: tokio::sync::mpsc::UnboundedSender<block::Hash>,
    chain_tip_sender: Arc<Mutex<ChainTipSender>>,
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

    while let Some(_block) = non_finalized_block_write_receiver.blocking_recv() {
        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // TODO:
        // - read from the channel
        // - commit blocks to the non-finalized state
        // - if there are any ready, commit blocks to the finalized state
        // - handle errors by sending a reset with all the block hashes in the non-finalized state, and the finalized tip
        // - update the chain tip sender and cached non-finalized state
        error!("handle non-finalized block writes here");
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
