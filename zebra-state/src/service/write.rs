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

        // Discard any children of invalid blocks in the channel
        let next_valid_height = finalized_state
            .db()
            .finalized_tip_height()
            .map(|height| (height + 1).expect("committed heights are valid"))
            .unwrap_or(Height(0));

        if ordered_block.0.height > next_valid_height {
            debug!(
                ?next_valid_height,
                invalid_height = ?ordered_block.0.height,
                invalid_hash = ?ordered_block.0.hash,
                "got a block that was too high, assuming that a parent block failed, \
                 and clearing child blocks in the channel",
            );

            let send_result =
                invalid_block_reset_sender.send(finalized_state.db().finalized_tip_hash());

            if send_result.is_err() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                return;
            }
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
                let finalized_tip = finalized_state.db().tip();

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
                    invalid_block_reset_sender.send(finalized_state.db().finalized_tip_hash());

                if send_result.is_err() {
                    info!("StateService closed the block reset channel. Is Zebra shutting down?");
                    return;
                }
            }
        }
    }

    while let Some(_block) = non_finalized_block_write_receiver.blocking_recv() {
        // TODO:
        // - read from the channel
        // - commit blocks to the non-finalized state
        // - commit blocks to the finalized state
        // - handle errors
        // - update the chain tip sender and cached non-finalized state
        error!("handle non-finalized block writes here");
    }
}
