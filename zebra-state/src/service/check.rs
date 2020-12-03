//! Consensus critical contextual checks

use std::borrow::Borrow;

use chrono::Duration;
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    parameters::POW_AVERAGING_WINDOW,
    work::difficulty::CompactDifficulty,
};

use crate::{PreparedBlock, ValidateContextError};

use super::check;

use difficulty::{AdjustedDifficulty, POW_MEDIAN_BLOCK_SPAN};

pub(crate) mod difficulty;

/// Check that `block` is contextually valid for `network`, based on the
/// `finalized_tip_height` and `relevant_chain`.
///
/// The relevant chain is an iterator over the ancestors of `block`, starting
/// with its parent block.
///
/// # Panics
///
/// If the state contains less than 28
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

    // The maximum number of blocks used by contextual checks
    const MAX_CONTEXT_BLOCKS: usize = POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN;
    let relevant_chain: Vec<_> = relevant_chain
        .into_iter()
        .take(MAX_CONTEXT_BLOCKS)
        .collect();
    assert_eq!(
        relevant_chain.len(),
        POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN,
        "state must contain enough blocks to do contextual validation"
    );

    let parent_block = relevant_chain
        .get(0)
        .expect("state must contain parent block to do contextual validation");
    let parent_block = parent_block.borrow();
    let parent_height = parent_block
        .coinbase_height()
        .expect("valid blocks have a coinbase height");
    check::height_one_more_than_parent_height(parent_height, prepared.height)?;

    let relevant_data = relevant_chain.iter().map(|block| {
        (
            block.borrow().header.difficulty_threshold,
            block.borrow().header.time,
        )
    });
    let difficulty_adjustment =
        AdjustedDifficulty::new_from_block(&prepared.block, network, relevant_data);
    check::difficulty_threshold_is_valid(
        prepared.block.header.difficulty_threshold,
        difficulty_adjustment,
    )?;

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

/// Validate the `time` and `difficulty_threshold` from a candidate block's
/// header.
///
/// Uses the `difficulty_adjustment` context for the block to:
///   * check that the the `time` field is within the valid range, and
///   * check that the expected difficulty is equal to the block's
///     `difficulty_threshold`.
///
/// These checks are performed together, because the time field is used to
/// calculate the expected difficulty adjustment.
fn difficulty_threshold_is_valid(
    difficulty_threshold: CompactDifficulty,
    difficulty_adjustment: AdjustedDifficulty,
) -> Result<(), ValidateContextError> {
    // Check the block header time consensus rules from the Zcash specification
    let median_time_past = difficulty_adjustment.median_time_past();
    let block_max_time_since_median = Duration::seconds(difficulty::BLOCK_MAX_TIME_SINCE_MEDIAN);
    if difficulty_adjustment.candidate_time() <= median_time_past {
        Err(ValidateContextError::TimeTooEarly)?
    } else if difficulty_adjustment.candidate_time()
        > median_time_past + block_max_time_since_median
    {
        Err(ValidateContextError::TimeTooLate)?
    }

    let expected_difficulty = difficulty_adjustment.expected_difficulty_threshold();
    if difficulty_threshold != expected_difficulty {
        Err(ValidateContextError::InvalidDifficultyThreshold)?
    }

    Ok(())
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
