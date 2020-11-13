use zebra_chain::block::{self, Block};

use crate::ValidateContextError;

use super::StateService;

pub(super) fn block_is_not_orphaned(
    service: &StateService,
    block: &Block,
) -> Result<(), ValidateContextError> {
    if block
        .coinbase_height()
        .expect("valid blocks have a coinbase height")
        <= service.sled.finalized_tip_height().expect(
            "finalized state must contain at least one block to use the non-finalized state",
        )
    {
        Err(ValidateContextError::OrphanedBlock)
    } else {
        Ok(())
    }
}

pub(super) fn height_one_more_than_parent_height(
    parent_height: block::Height,
    block: &Block,
) -> Result<(), ValidateContextError> {
    let height = block
        .coinbase_height()
        .expect("valid blocks have a coinbase height");

    if parent_height + 1 != Some(height) {
        Err(ValidateContextError::NonSequentialBlock)
    } else {
        Ok(())
    }
}
