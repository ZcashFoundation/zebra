//! Consensus critical contextual checks

use zebra_chain::block::{self, Block};

use crate::ValidateContextError;

/// Returns `ValidateContextError::OrphanedBlock` if the height of the given
/// block is less than or equal to the finalized tip height.
pub(super) fn block_is_not_orphaned(
    finalized_tip_height: block::Height,
    block: &Block,
) -> Result<(), ValidateContextError> {
    if block
        .coinbase_height()
        .expect("valid blocks have a coinbase height")
        <= finalized_tip_height
    {
        Err(ValidateContextError::OrphanedBlock)
    } else {
        Ok(())
    }
}

/// Returns `ValidateContextError::NonSequentialBlock` if the block height isn't
/// equal to the parent_height+1.
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use zebra_chain::serialization::ZcashDeserializeInto;

    use super::*;

    #[test]
    fn test_orphan_consensus_check() {
        zebra_test::init();

        let block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();

        block_is_not_orphaned(block::Height(0), &block).expect("tip is lower so it should be fine");
        block_is_not_orphaned(block::Height(347498), &block)
            .expect("tip is lower so it should be fine");
        block_is_not_orphaned(block::Height(347499), &block)
            .expect_err("tip is equal so it should error");
        block_is_not_orphaned(block::Height(500000), &block)
            .expect_err("tip is higher so it should error");
    }

    #[test]
    fn test_sequential_height_check() {
        zebra_test::init();

        let block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();

        height_one_more_than_parent_height(block::Height(0), &block)
            .expect_err("block is much lower, should panic");
        height_one_more_than_parent_height(block::Height(347497), &block)
            .expect_err("parent height is 2 less, should panic");
        height_one_more_than_parent_height(block::Height(347498), &block)
            .expect("parent height is 1 less, should be good");
        height_one_more_than_parent_height(block::Height(347499), &block)
            .expect_err("parent height is equal, should panic");
        height_one_more_than_parent_height(block::Height(347500), &block)
            .expect_err("parent height is way more, should panic");
        height_one_more_than_parent_height(block::Height(500000), &block)
            .expect_err("parent height is way more, should panic");
    }
}
