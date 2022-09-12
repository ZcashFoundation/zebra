//! Writing blocks to the finalized and non-finalized states.

use crate::{service::finalized_state::FinalizedState, FinalizedBlock, PreparedBlock};

/// Write blocks to the state,
///
/// TODO: pass the non-finalized state and associated update channels to this function
pub fn write_blocks_from_channels(
    mut finalized_block_write_receiver: tokio::sync::mpsc::UnboundedReceiver<FinalizedBlock>,
    mut non_finalized_block_write_receiver: tokio::sync::mpsc::UnboundedReceiver<PreparedBlock>,
    _finalized_state: FinalizedState,
) {
    // TODO: actually write blocks here
    while let Some(_block) = finalized_block_write_receiver.blocking_recv() {
        error!("handle finalized block writes here");
    }

    while let Some(_block) = non_finalized_block_write_receiver.blocking_recv() {
        error!("handle non-finalized block writes here");
    }
}
