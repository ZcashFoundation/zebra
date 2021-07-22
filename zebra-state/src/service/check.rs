//! Consensus critical contextual checks

use std::borrow::Borrow;

use chrono::Duration;

use zebra_chain::{
    block::{self, Block},
    parameters::POW_AVERAGING_WINDOW,
    parameters::{Network, NetworkUpgrade},
    work::difficulty::CompactDifficulty,
};

use crate::{PreparedBlock, ValidateContextError};

use super::check;

use difficulty::{AdjustedDifficulty, POW_MEDIAN_BLOCK_SPAN};

pub(crate) mod difficulty;
pub(crate) mod nullifier;
pub(crate) mod utxo;

#[cfg(test)]
mod tests;

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

    let parent_block = relevant_chain
        .get(0)
        .expect("state must contain parent block to do contextual validation");
    let parent_block = parent_block.borrow();
    let parent_height = parent_block
        .coinbase_height()
        .expect("valid blocks have a coinbase height");
    check::height_one_more_than_parent_height(parent_height, prepared.height)?;

    // skip this check during tests if we don't have enough blocks in the chain
    #[cfg(test)]
    if relevant_chain.len() < POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN {
        return Ok(());
    }
    // process_queued also checks the chain length, so we can skip this assertion during testing
    // (tests that want to check this code should use the correct number of blocks)
    assert_eq!(
        relevant_chain.len(),
        POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN,
        "state must contain enough blocks to do proof of work contextual validation, \
         and validation must receive the exact number of required blocks"
    );

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

    Ok(())
}

/// Returns `ValidateContextError::OrphanedBlock` if the height of the given
/// block is less than or equal to the finalized tip height.
fn block_is_not_orphaned(
    finalized_tip_height: block::Height,
    candidate_height: block::Height,
) -> Result<(), ValidateContextError> {
    if candidate_height <= finalized_tip_height {
        Err(ValidateContextError::OrphanedBlock {
            candidate_height,
            finalized_tip_height,
        })
    } else {
        Ok(())
    }
}

/// Returns `ValidateContextError::NonSequentialBlock` if the block height isn't
/// equal to the parent_height+1.
fn height_one_more_than_parent_height(
    parent_height: block::Height,
    candidate_height: block::Height,
) -> Result<(), ValidateContextError> {
    if parent_height + 1 != Some(candidate_height) {
        Err(ValidateContextError::NonSequentialBlock {
            candidate_height,
            parent_height,
        })
    } else {
        Ok(())
    }
}

/// Validate the time and `difficulty_threshold` from a candidate block's
/// header.
///
/// Uses the `difficulty_adjustment` context for the block to:
///   * check that the candidate block's time is within the valid range,
///     based on the network and  candidate height, and
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
    let candidate_height = difficulty_adjustment.candidate_height();
    let candidate_time = difficulty_adjustment.candidate_time();
    let network = difficulty_adjustment.network();
    let median_time_past = difficulty_adjustment.median_time_past();
    let block_time_max =
        median_time_past + Duration::seconds(difficulty::BLOCK_MAX_TIME_SINCE_MEDIAN);
    if candidate_time <= median_time_past {
        Err(ValidateContextError::TimeTooEarly {
            candidate_time,
            median_time_past,
        })?
    }

    // The maximum time rule is only active on Testnet from a specific height
    if NetworkUpgrade::is_max_block_time_enforced(network, candidate_height)
        && candidate_time > block_time_max
    {
        Err(ValidateContextError::TimeTooLate {
            candidate_time,
            block_time_max,
        })?
    }

    let expected_difficulty = difficulty_adjustment.expected_difficulty_threshold();
    if difficulty_threshold != expected_difficulty {
        Err(ValidateContextError::InvalidDifficultyThreshold {
            difficulty_threshold,
            expected_difficulty,
        })?
    }

    Ok(())
}
