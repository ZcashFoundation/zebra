//! Consensus critical contextual checks

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use zebra_chain::{
    block::{
        self, merkle::AuthDataRoot, Block, ChainHistoryBlockTxAuthCommitmentHash, CommitmentError,
    },
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade},
    work::difficulty::CompactDifficulty,
};

use crate::{
    service::{
        block_iter::any_chain_ancestor_iter,
        check::difficulty::{pow_adjustment_block_span, MAX_POW_ADJUSTMENT_BLOCK_SPAN},
        finalized_state::ZebraDb,
        non_finalized_state::NonFinalizedState,
    },
    BoxError, SemanticallyVerifiedBlock, ValidateContextError,
};

use zebra_chain::{
    amount::{Amount, NonNegative},
    parameters::subsidy::{self, CoinbaseTransactionError, SubsidyError},
    value_balance::ValueBalance,
};

use crate::ContextuallyVerifiedBlock;

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

#[cfg(test)]
thread_local! {
    /// The number of times this thread read a block's recent headers from the state.
    pub(crate) static RECENT_HEADERS_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// The difficulty thresholds and times of a block and its most recent ancestors: the header
/// fields that the contextual checks of the block's children use.
///
/// Reading them from the state visits each of those blocks. A block's recent headers are its
/// own fields followed by its parent's recent headers, so a caller that validates a chain block
/// by block can keep them instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecentHeaders {
    /// The hash of the newest block.
    tip: block::Hash,

    /// The height of the newest block.
    tip_height: block::Height,

    /// The difficulty threshold and time of the newest block and its ancestors, newest first.
    ///
    /// Holds at most [`MAX_POW_ADJUSTMENT_BLOCK_SPAN`] blocks.
    headers: Vec<(CompactDifficulty, DateTime<Utc>)>,
}

impl RecentHeaders {
    /// Reads the recent headers of the block with the hash `tip` from the state: `block_span`
    /// blocks, or fewer if the state has no more headers.
    ///
    /// Returns `None` if the state doesn't have that block's header.
    pub(crate) fn read(
        non_finalized_state: &NonFinalizedState,
        finalized_state: &ZebraDb,
        tip: block::Hash,
        block_span: usize,
    ) -> Option<Self> {
        #[cfg(test)]
        RECENT_HEADERS_READS.with(|reads| reads.set(reads.get() + 1));

        let relevant_chain =
            any_chain_ancestor_iter::<block::Header>(non_finalized_state, finalized_state, tip);
        let tip_height = relevant_chain.height?;
        let headers: Vec<_> = relevant_chain
            .take(block_span)
            .map(|header| (header.difficulty_threshold, header.time))
            .collect();

        (!headers.is_empty()).then_some(Self {
            tip,
            tip_height,
            headers,
        })
    }

    /// Returns `true` if these are all the headers that a read of `block_span` blocks returns:
    /// that many blocks, or every block down to the genesis block.
    fn covers(&self, block_span: usize) -> bool {
        self.headers.len() >= block_span || self.headers.len() > self.tip_height.as_usize()
    }

    /// Returns the height and hash of the newest block.
    pub(crate) fn tip(&self) -> (block::Height, block::Hash) {
        (self.tip_height, self.tip)
    }

    /// Returns the difficulty thresholds and times of the newest `block_span` blocks, newest
    /// first, or `None` if a read of that many blocks could return more than these.
    pub(crate) fn newest(
        &self,
        block_span: usize,
    ) -> Option<Vec<(CompactDifficulty, DateTime<Utc>)>> {
        self.covers(block_span)
            .then(|| self.headers.iter().take(block_span).copied().collect())
    }

    /// Returns the recent headers of `child`, or `None` if it isn't a child of this newest block.
    pub(crate) fn of_child(&self, child: &SemanticallyVerifiedBlock) -> Option<Self> {
        (child.block.header.previous_block_hash == self.tip).then(|| self.with_child(child))
    }

    /// Returns the recent headers of `child`, which must be a child of this newest block.
    fn with_child(&self, child: &SemanticallyVerifiedBlock) -> Self {
        let mut headers = Vec::with_capacity(MAX_POW_ADJUSTMENT_BLOCK_SPAN);
        headers.push((
            child.block.header.difficulty_threshold,
            child.block.header.time,
        ));
        headers.extend(
            self.headers
                .iter()
                .take(MAX_POW_ADJUSTMENT_BLOCK_SPAN - 1)
                .copied(),
        );

        Self {
            tip: child.hash,
            tip_height: child.height,
            headers,
        }
    }
}

/// Check that the semantically verified block is contextually valid for `network`,
/// based on the recent headers of its parent.
///
/// This function performs checks that require a small number of recent headers,
/// including previous height and block difficulty.
#[tracing::instrument(skip(semantically_verified, parent_headers))]
fn block_is_valid_for_recent_chain(
    semantically_verified: &SemanticallyVerifiedBlock,
    network: &Network,
    parent_headers: &RecentHeaders,
) -> Result<(), ValidateContextError> {
    check::height_one_more_than_parent_height(
        parent_headers.tip_height,
        semantically_verified.height,
    )?;

    // skip this check during tests if we don't have enough blocks in the chain
    // process_queued also checks the chain length, so we can skip this assertion during testing
    // (tests that want to check this code should use the correct number of blocks)
    //
    // TODO: accept a NotReadyToBeCommitted error in those tests instead
    #[cfg(test)]
    if parent_headers.headers.len()
        < pow_adjustment_block_span(network, semantically_verified.height)
    {
        return Ok(());
    }

    // Recent headers always hold the parent's, so there is some context for the block.
    //
    // It's okay to use a relevant chain of fewer than the block span, because
    // the MedianTime function uses height 0 if passed a negative height by the ActualTimespan function:
    // > ActualTimespan(height : N) := MedianTime(height) − MedianTime(height − PoWAveragingWindow)
    // > MedianTime(height : N) := median([[ nTime(𝑖) for 𝑖 from max(0, height − PoWMedianBlockSpan) up to height − 1 ]])
    // and the MeanTarget function only requires the past `PoWAveragingWindow` (17) blocks for heights above 17,
    // > PoWLimit, if height ≤ PoWAveragingWindow
    // > ([ToTarget(nBits(𝑖)) for 𝑖 from height−PoWAveragingWindow up to height−1]) otherwise
    //
    // See the 'Difficulty Adjustment' section (page 132) in the Zcash specification.
    let difficulty_adjustment = AdjustedDifficulty::new_from_block(
        &semantically_verified.block,
        network,
        parent_headers.headers.iter().copied(),
    );
    check::difficulty_threshold_and_time_are_valid(
        semantically_verified.block.header.difficulty_threshold,
        difficulty_adjustment,
    )?;

    Ok(())
}

/// Check that `block` is contextually valid for `network`, using
/// the `history_tree` up to and including the previous block.
///
/// Any precomputed root must come from the same immutable checkpoint block body.
#[tracing::instrument(skip(block, history_tree))]
pub(crate) fn block_commitment_is_valid_for_chain_history(
    block: Arc<Block>,
    network: &Network,
    history_tree: &HistoryTree,
    precomputed_auth_data_root: Option<AuthDataRoot>,
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
            // Portions copyright (c) 2026 Zakura Contributors, licensed under the MIT license or
            // the Apache License, Version 2.0, at your option.
            // Adapted from zakura-core/zakura
            // (https://github.com/zakura-core/zakura/commit/3482b607a4008da9d2a6833243959d88f28a0659).
            // Adapted from zakura-core/zakura
            // (https://github.com/zakura-core/zakura/commit/21107f19f7a3dd3f125cff4d565bc45d8ca91be4).
            let auth_data_root =
                precomputed_auth_data_root.unwrap_or_else(|| block.auth_data_root());

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
pub(crate) fn difficulty_threshold_and_time_are_valid(
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

/// Checks the cached chain and returns the first block with observed transaction branch IDs.
///
/// `nu5_activation_height` should be `NetworkUpgrade::Nu5.activation_height(network)`, and
/// `max_legacy_chain_blocks` should be [`MAX_LEGACY_CHAIN_BLOCKS`](crate::constants::MAX_LEGACY_CHAIN_BLOCKS).
/// They are only changed from the defaults for testing.
///
/// Returns `None` only after reaching a pre-NU5 block. Missing bodies or an exhausted iterator
/// cannot establish branch consistency and fail closed.
pub(crate) fn legacy_chain<I>(
    nu5_activation_height: block::Height,
    ancestors: I,
    network: &Network,
    max_legacy_chain_blocks: usize,
) -> Result<Option<block::Height>, BoxError>
where
    I: Iterator<Item = Result<Arc<Block>, crate::BlockReadError>>,
{
    let mut ancestors = ancestors.peekable();
    let tip_height = ancestors
        .peek()
        .and_then(|block| block.as_ref().ok())
        .and_then(|block| block.coinbase_height());

    for (index, block) in ancestors.enumerate() {
        let block = block.map_err(|error| {
            format!(
                "cannot prove the cached chain follows the configured network upgrade: {error}; \
                 restore an archive snapshot or resync"
            )
        })?;
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
            return Ok(None);
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
            return Ok(block.coinbase_height());
        }
    }

    Err(
        "cannot prove the cached chain follows the configured network upgrade: \
         ancestor history ended before finding a transaction branch ID or a pre-NU5 block; \
         restore an archive snapshot or resync"
            .into(),
    )
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
    // `non_finalized_state` can be a published copy of the block write task's state, which can
    // still hold blocks that the database has finalized since. So the finalized tip's height and
    // the block's ancestors are read from the state.
    contextual_validity(
        finalized_state,
        non_finalized_state,
        semantically_verified,
        None,
        finalized_state.finalized_tip_height(),
    )
    .map(|_recent_headers| ())
}

/// Runs the checks of [`initial_contextual_validity`] with the height of the finalized tip that
/// `non_finalized_state` builds on.
///
/// The headers of the block's ancestors are taken from `recent_headers` if they are the recent
/// headers of the block's parent, and read from the state otherwise.
///
/// Returns the block's own recent headers, for the checks of its children.
pub(crate) fn contextual_validity(
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
    semantically_verified: &SemanticallyVerifiedBlock,
    recent_headers: Option<&RecentHeaders>,
    finalized_tip_height: Option<block::Height>,
) -> Result<RecentHeaders, ValidateContextError> {
    let finalized_tip_height = finalized_tip_height
        .expect("finalized state must contain at least one block to do contextual validation");
    // A block at or below the finalized tip is rejected before its ancestors are read.
    check::block_is_not_orphaned(finalized_tip_height, semantically_verified.height)?;

    let parent_hash = semantically_verified.block.header.previous_block_hash;
    // ZIP 218 makes the averaging window, and so the block span, depend on the block's height.
    let block_span =
        pow_adjustment_block_span(&non_finalized_state.network, semantically_verified.height);

    // A read only finds the headers of a parent that is in the state, so kept headers are only
    // used for a parent that is still in a chain.
    let kept_headers = recent_headers.filter(|recent_headers| {
        recent_headers.tip == parent_hash
            && recent_headers.covers(block_span)
            && non_finalized_state.any_chain_contains(&parent_hash)
    });
    let read_headers = match kept_headers {
        Some(_) => None,
        None => RecentHeaders::read(
            non_finalized_state,
            finalized_state,
            parent_hash,
            block_span,
        ),
    };
    let Some(parent_headers) = kept_headers.or(read_headers.as_ref()) else {
        warn!(
            ?semantically_verified,
            ?finalized_tip_height,
            "state must contain parent block to do contextual validation"
        );

        return Err(ValidateContextError::NotReadyToBeCommitted);
    };

    // Security: check proof of work before any other checks
    check::block_is_valid_for_recent_chain(
        semantically_verified,
        &non_finalized_state.network,
        parent_headers,
    )?;

    check::nullifier::no_duplicates_in_finalized_chain(semantically_verified, finalized_state)?;

    Ok(parent_headers.with_child(semantically_verified))
}

/// Checks the block subsidy, funding streams, and miner fees paid by `contextual`'s coinbase
/// transaction, from the NSM reissuance height.
///
/// The block verifier checks the subsidy, funding streams and miner fees before the deployment
/// height. From then on the block subsidy depends on the NSM value balance after the parent block,
/// which is only known during contextual validation, so the checks run here instead.
///
/// [zip]: https://zips.z.cash/zip-0237
pub(crate) fn nsm_subsidy_is_valid(
    contextual: &ContextuallyVerifiedBlock,
    network: &Network,
    parent_chain_value_pools: ValueBalance<NonNegative>,
    block_miner_fees: Amount<NonNegative>,
) -> Result<(), ValidateContextError> {
    let invalid_subsidy =
        |subsidy_error: CoinbaseTransactionError| ValidateContextError::InvalidSubsidy {
            subsidy_error,
            height: contextual.height,
            block_hash: contextual.hash,
        };

    let expected_block_subsidy = subsidy::block_subsidy_with_parent_pools(
        contextual.height,
        network,
        parent_chain_value_pools,
    )
    .map_err(|error| invalid_subsidy(error.into()))?;

    let deferred_pool_balance_change =
        subsidy::subsidy_is_valid(&contextual.block, network, expected_block_subsidy)
            .map_err(invalid_subsidy)?;

    let coinbase_tx = contextual
        .block
        .transactions
        .first()
        .ok_or_else(|| invalid_subsidy(SubsidyError::NoCoinbase.into()))?;

    subsidy::miner_fees_are_valid(
        coinbase_tx,
        contextual.height,
        block_miner_fees,
        expected_block_subsidy,
        deferred_pool_balance_change,
        network,
    )
    .map_err(invalid_subsidy)
}
