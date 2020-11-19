//! Consensus critical contextual checks

use std::borrow::Borrow;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    parameters::POW_AVERAGING_WINDOW,
    work::difficulty::CompactDifficulty,
};

use crate::{PreparedBlock, ValidateContextError};

use super::check;

pub mod difficulty;
use difficulty::{AdjustedDifficulty, POW_MEDIAN_BLOCK_SPAN};

/// Check that `block` is contextually valid for `network`, based on the
/// `finalized_tip_height` and `relevant_chain`.
///
/// The relevant chain is an iterator over the ancestors of `block`, starting
/// with its parent block.
///
/// Panics if the finalized state is empty.
///
/// Skips the difficulty adjustment check if the state contains less than 28
/// (`POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN`) blocks.
#[tracing::instrument(
    name = "contextual_validation",
    fields(?network),
    skip(prepared, network, finalized_tip_height, relevant_chain)
)]
pub(crate) fn block_is_contextually_valid<C>(
    prepared: &PreparedBlock,
    network: Network,
    finalized_tip_height: Option<block::Height>,
    relevant_chain: C,
) -> Result<(), ValidateContextError>
where
    C: IntoIterator,
    C::Item: Borrow<Block>,
    C::IntoIter: ExactSizeIterator,
{
    let finalized_tip_height = finalized_tip_height
        .expect("finalized state must contain at least one block to do contextual validation");
    check::block_is_not_orphaned(finalized_tip_height, prepared.height)?;

    // Peek at the first block
    let mut relevant_chain = relevant_chain.into_iter().peekable();
    let parent_block = relevant_chain
        .peek()
        .expect("state must contain parent block to do contextual validation");
    let parent_block = parent_block.borrow();
    let parent_height = parent_block
        .coinbase_height()
        .expect("valid blocks have a coinbase height");
    check::height_one_more_than_parent_height(parent_height, prepared.height)?;

    // Note: the difficulty check reads the first 28 blocks from the relevant
    // chain iterator. If you want to use those blocks in other checks, you'll
    // need to clone them here.

    if relevant_chain.len() >= POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN {
        let relevant_data = relevant_chain.map(|block| {
            (
                block.borrow().header.difficulty_threshold,
                block.borrow().header.time,
            )
        });
        // Reads the first 28 blocks from the iterator
        let expected_difficulty =
            AdjustedDifficulty::new_from_block(&prepared.block, network, relevant_data);
        check::difficulty_threshold_is_valid(
            prepared.block.header.difficulty_threshold,
            expected_difficulty,
        )?;
    }

    // TODO: other contextual validation design and implementation
    Ok(())
}

/// Returns `ValidateContextError::OrphanedBlock` if the height of the given
/// block is less than or equal to the finalized tip height.
fn block_is_not_orphaned(
    finalized_tip_height: block::Height,
    height: block::Height,
) -> Result<(), ValidateContextError> {
    if height <= finalized_tip_height {
        Err(ValidateContextError::OrphanedBlock)
    } else {
        Ok(())
    }
}

/// Returns `ValidateContextError::NonSequentialBlock` if the block height isn't
/// equal to the parent_height+1.
fn height_one_more_than_parent_height(
    parent_height: block::Height,
    height: block::Height,
) -> Result<(), ValidateContextError> {
    if parent_height + 1 != Some(height) {
        Err(ValidateContextError::NonSequentialBlock)
    } else {
        Ok(())
    }
}

/// Validate the `difficulty_threshold` from a candidate block's header, based
/// on an `expected_difficulty` for that block.
///
/// Uses `expected_difficulty` to calculate the expected difficulty value, then
/// compares that value to the `difficulty_threshold`.
///
/// The check passes if the values are equal.
fn difficulty_threshold_is_valid(
    difficulty_threshold: CompactDifficulty,
    expected_difficulty: AdjustedDifficulty,
) -> Result<(), ValidateContextError> {
    if difficulty_threshold == expected_difficulty.expected_difficulty_threshold() {
        Ok(())
    } else {
        Err(ValidateContextError::InvalidDifficultyThreshold)
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

        let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap()
            .coinbase_height()
            .unwrap();

        block_is_not_orphaned(block::Height(0), height).expect("tip is lower so it should be fine");
        block_is_not_orphaned(block::Height(347498), height)
            .expect("tip is lower so it should be fine");
        block_is_not_orphaned(block::Height(347499), height)
            .expect_err("tip is equal so it should error");
        block_is_not_orphaned(block::Height(500000), height)
            .expect_err("tip is higher so it should error");
    }

    #[test]
    fn test_sequential_height_check() {
        zebra_test::init();

        let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap()
            .coinbase_height()
            .unwrap();

        height_one_more_than_parent_height(block::Height(0), height)
            .expect_err("block is much lower, should panic");
        height_one_more_than_parent_height(block::Height(347497), height)
            .expect_err("parent height is 2 less, should panic");
        height_one_more_than_parent_height(block::Height(347498), height)
            .expect("parent height is 1 less, should be good");
        height_one_more_than_parent_height(block::Height(347499), height)
            .expect_err("parent height is equal, should panic");
        height_one_more_than_parent_height(block::Height(347500), height)
            .expect_err("parent height is way more, should panic");
        height_one_more_than_parent_height(block::Height(500000), height)
            .expect_err("parent height is way more, should panic");
    }
}
