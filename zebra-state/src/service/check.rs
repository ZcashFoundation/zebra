//! Consensus critical contextual checks

use std::{borrow::Borrow, sync::Arc};

use chrono::Duration;

use zebra_chain::{
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash, CommitmentError},
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade},
    work::difficulty::CompactDifficulty,
};

use crate::{
    service::{
        block_iter::any_ancestor_blocks, check::difficulty::POW_ADJUSTMENT_BLOCK_SPAN,
        finalized_state::ZebraDb, non_finalized_state::NonFinalizedState,
    },
    BoxError, SemanticallyVerifiedBlock, ValidateContextError,
};

// use self as check
use super::check;

// These types are used in doc links
#[allow(unused_imports)]
use crate::service::non_finalized_state::Chain;

pub(crate) mod anchors;
pub(crate) mod difficulty;
pub(crate) mod nullifier;
pub(crate) mod utxo;

pub use utxo::transparent_coinbase_spend;

#[cfg(test)]
mod tests;

pub(crate) use difficulty::AdjustedDifficulty;

/// Check that the semantically verified block is contextually valid for `network`,
/// based on the `finalized_tip_height` and `relevant_chain`.
///
/// This function performs checks that require a small number of recent blocks,
/// including previous hash, previous height, and block difficulty.
///
/// The relevant chain is an iterator over the ancestors of `block`, starting
/// with its parent block.
#[tracing::instrument(skip(semantically_verified, finalized_tip_height, relevant_chain))]
pub(crate) fn block_is_valid_for_recent_chain<C>(
    semantically_verified: &SemanticallyVerifiedBlock,
    network: &Network,
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
    check::block_is_not_orphaned(finalized_tip_height, semantically_verified.height)?;

    let relevant_chain: Vec<_> = relevant_chain
        .into_iter()
        .take(POW_ADJUSTMENT_BLOCK_SPAN)
        .collect();

    let Some(parent_block) = relevant_chain.first() else {
        warn!(
            ?semantically_verified,
            ?finalized_tip_height,
            "state must contain parent block to do contextual validation"
        );

        return Err(ValidateContextError::NotReadyToBeCommitted);
    };

    let parent_block = parent_block.borrow();
    let parent_height = parent_block
        .coinbase_height()
        .expect("valid blocks have a coinbase height");
    check::height_one_more_than_parent_height(parent_height, semantically_verified.height)?;

    // skip this check during tests if we don't have enough blocks in the chain
    // process_queued also checks the chain length, so we can skip this assertion during testing
    // (tests that want to check this code should use the correct number of blocks)
    //
    // TODO: accept a NotReadyToBeCommitted error in those tests instead
    #[cfg(test)]
    if relevant_chain.len() < POW_ADJUSTMENT_BLOCK_SPAN {
        return Ok(());
    }

    // In production, blocks without enough context are invalid.
    //
    // The BlockVerifierRouter makes sure that the first 1 million blocks (or more) are
    // checkpoint verified. The state queues and block write task make sure that blocks are
    // committed in strict height order. But this function is only called on semantically
    // verified blocks, so there will be at least 1 million blocks in the state when it is
    // called. So this error should never happen on Mainnet or the default Testnet.
    //
    // It's okay to use a relevant chain of fewer than `POW_ADJUSTMENT_BLOCK_SPAN` blocks, because
    // the MedianTime function uses height 0 if passed a negative height by the ActualTimespan function:
    // > ActualTimespan(height : N) := MedianTime(height) − MedianTime(height − PoWAveragingWindow)
    // > MedianTime(height : N) := median([[ nTime(𝑖) for 𝑖 from max(0, height − PoWMedianBlockSpan) up to height − 1 ]])
    // and the MeanTarget function only requires the past `PoWAveragingWindow` (17) blocks for heights above 17,
    // > PoWLimit, if height ≤ PoWAveragingWindow
    // > ([ToTarget(nBits(𝑖)) for 𝑖 from height−PoWAveragingWindow up to height−1]) otherwise
    //
    // See the 'Difficulty Adjustment' section (page 132) in the Zcash specification.
    #[cfg(not(test))]
    if relevant_chain.is_empty() {
        return Err(ValidateContextError::NotReadyToBeCommitted);
    }

    let relevant_data = relevant_chain.iter().map(|block| {
        (
            block.borrow().header.difficulty_threshold,
            block.borrow().header.time,
        )
    });
    let difficulty_adjustment =
        AdjustedDifficulty::new_from_block(&semantically_verified.block, network, relevant_data);
    check::difficulty_threshold_and_time_are_valid(
        semantically_verified.block.header.difficulty_threshold,
        difficulty_adjustment,
    )?;

    Ok(())
}

/// Check that `block` is contextually valid for `network`, using
/// the `history_tree` up to and including the previous block.
#[tracing::instrument(skip(block, history_tree))]
pub(crate) fn block_commitment_is_valid_for_chain_history(
    block: Arc<Block>,
    network: &Network,
    history_tree: &HistoryTree,
) -> Result<(), ValidateContextError> {
    match block.commitment(network)? {
        block::Commitment::PreSaplingReserved(_)
        | block::Commitment::FinalSaplingRoot(_)
        | block::Commitment::ChainHistoryActivationReserved => {
            // # Consensus
            //
            // > [Sapling and Blossom only, pre-Heartwood] hashLightClientRoot MUST
            // > be LEBS2OSP_{256}(rt^{Sapling}) where rt^{Sapling} is the root of
            // > the Sapling note commitment tree for the final Sapling treestate of
            // > this block .
            //
            // https://zips.z.cash/protocol/protocol.pdf#blockheader
            //
            // We don't need to validate this rule since we checkpoint on Canopy.
            //
            // We also don't need to do anything in the other cases.
            Ok(())
        }
        block::Commitment::ChainHistoryRoot(actual_history_tree_root) => {
            // # Consensus
            //
            // > [Heartwood and Canopy only, pre-NU5] hashLightClientRoot MUST be set to the
            // > hashChainHistoryRoot for this block , as specified in [ZIP-221].
            //
            // https://zips.z.cash/protocol/protocol.pdf#blockheader
            //
            // The network is checked by [`Block::commitment`] above; it will only
            // return the chain history root if it's Heartwood or Canopy.
            let history_tree_root = history_tree
                .hash()
                .expect("the history tree of the previous block must exist since the current block has a ChainHistoryRoot");
            if actual_history_tree_root == history_tree_root {
                Ok(())
            } else {
                Err(ValidateContextError::InvalidBlockCommitment(
                    CommitmentError::InvalidChainHistoryRoot {
                        actual: actual_history_tree_root.into(),
                        expected: history_tree_root.into(),
                    },
                ))
            }
        }
        block::Commitment::ChainHistoryBlockTxAuthCommitment(actual_hash_block_commitments) => {
            // # Consensus
            //
            // > [NU5 onward] hashBlockCommitments MUST be set to the value of
            // > hashBlockCommitments for this block, as specified in [ZIP-244].
            //
            // The network is checked by [`Block::commitment`] above; it will only
            // return the block commitments if it's NU5 onward.
            let history_tree_root = history_tree
                .hash()
                .or_else(|| {
                    (NetworkUpgrade::Heartwood.activation_height(network)
                        == block.coinbase_height())
                    .then_some(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into())
                })
                .expect(
                    "the history tree of the previous block must exist \
                 since the current block has a ChainHistoryBlockTxAuthCommitment",
                );
            let auth_data_root = block.auth_data_root();

            let hash_block_commitments = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &history_tree_root,
                &auth_data_root,
            );

            if actual_hash_block_commitments == hash_block_commitments {
                Ok(())
            } else {
                Err(ValidateContextError::InvalidBlockCommitment(
                    CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                        actual: actual_hash_block_commitments.into(),
                        expected: hash_block_commitments.into(),
                    },
                ))
            }
        }
    }
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
fn difficulty_threshold_and_time_are_valid(
    difficulty_threshold: CompactDifficulty,
    difficulty_adjustment: AdjustedDifficulty,
) -> Result<(), ValidateContextError> {
    // Check the block header time consensus rules from the Zcash specification
    let candidate_height = difficulty_adjustment.candidate_height();
    let candidate_time = difficulty_adjustment.candidate_time();
    let network = difficulty_adjustment.network();
    let median_time_past = difficulty_adjustment.median_time_past();
    let block_time_max =
        median_time_past + Duration::seconds(difficulty::BLOCK_MAX_TIME_SINCE_MEDIAN.into());

    // # Consensus
    //
    // > For each block other than the genesis block, `nTime` MUST be strictly greater
    // than the median-time-past of that block.
    //
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let genesis_height = NetworkUpgrade::Genesis
        .activation_height(&network)
        .expect("Zebra always has a genesis height available");

    if candidate_time <= median_time_past && candidate_height != genesis_height {
        Err(ValidateContextError::TimeTooEarly {
            candidate_time,
            median_time_past,
        })?
    }

    // # Consensus
    //
    // > For each block at block height 2 or greater on Mainnet, or block height 653_606
    // or greater on Testnet, `nTime` MUST be less than or equal to the median-time-past
    // of that block plus 90*60 seconds.
    //
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    if network.is_max_block_time_enforced(candidate_height) && candidate_time > block_time_max {
        Err(ValidateContextError::TimeTooLate {
            candidate_time,
            block_time_max,
        })?
    }

    // # Consensus
    //
    // > For a block at block height `Height`, `nBits` MUST be equal to `ThresholdBits(Height)`.
    //
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let expected_difficulty = difficulty_adjustment.expected_difficulty_threshold();
    if difficulty_threshold != expected_difficulty {
        Err(ValidateContextError::InvalidDifficultyThreshold {
            difficulty_threshold,
            expected_difficulty,
        })?
    }

    Ok(())
}

/// Check if zebra is following a legacy chain and return an error if so.
///
/// `nu5_activation_height` should be `NetworkUpgrade::Nu5.activation_height(network)`, and
/// `max_legacy_chain_blocks` should be [`MAX_LEGACY_CHAIN_BLOCKS`](crate::constants::MAX_LEGACY_CHAIN_BLOCKS).
/// They are only changed from the defaults for testing.
pub(crate) fn legacy_chain<I>(
    nu5_activation_height: block::Height,
    ancestors: I,
    network: &Network,
    max_legacy_chain_blocks: usize,
) -> Result<(), BoxError>
where
    I: Iterator<Item = Arc<Block>>,
{
    let mut ancestors = ancestors.peekable();
    let tip_height = ancestors.peek().and_then(|block| block.coinbase_height());

    for (index, block) in ancestors.enumerate() {
        // Stop checking if the chain reaches Canopy. We won't find any more V5 transactions,
        // so the rest of our checks are useless.
        //
        // If the cached tip is close to NU5 activation, but there aren't any V5 transactions in the
        // chain yet, we could reach MAX_BLOCKS_TO_CHECK in Canopy, and incorrectly return an error.
        if block
            .coinbase_height()
            .expect("valid blocks have coinbase heights")
            < nu5_activation_height
        {
            return Ok(());
        }

        // If we are past our NU5 activation height, but there are no V5 transactions in recent blocks,
        // the last Zebra instance that updated this cached state had no NU5 activation height.
        if index >= max_legacy_chain_blocks {
            return Err(format!(
                "could not find any transactions in recent blocks: \
                 checked {index} blocks back from {:?}",
                tip_height.expect("database contains valid blocks"),
            )
            .into());
        }

        // If a transaction `network_upgrade` field is different from the network upgrade calculated
        // using our activation heights, the Zebra instance that verified those blocks had different
        // network upgrade heights.
        block
            .check_transaction_network_upgrade_consistency(network)
            .map_err(|error| {
                format!("inconsistent network upgrade found in transaction: {error:?}")
            })?;

        // If we find at least one transaction with a valid `network_upgrade` field, the Zebra instance that
        // verified those blocks used the same network upgrade heights. (Up to this point in the chain.)
        let has_network_upgrade = block
            .transactions
            .iter()
            .find_map(|trans| trans.network_upgrade())
            .is_some();
        if has_network_upgrade {
            return Ok(());
        }
    }

    Ok(())
}

/// Perform initial contextual validity checks for the configured network,
/// based on the committed finalized and non-finalized state.
///
/// Additional contextual validity checks are performed by the non-finalized [`Chain`].
pub(crate) fn initial_contextual_validity(
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<(), ValidateContextError> {
    let relevant_chain = any_ancestor_blocks(
        non_finalized_state,
        finalized_state,
        semantically_verified.block.header.previous_block_hash,
    );

    // Security: check proof of work before any other checks
    check::block_is_valid_for_recent_chain(
        semantically_verified,
        &non_finalized_state.network,
        finalized_state.finalized_tip_height(),
        relevant_chain,
    )?;

    check::nullifier::no_duplicates_in_finalized_chain(semantically_verified, finalized_state)?;

    Ok(())
}
