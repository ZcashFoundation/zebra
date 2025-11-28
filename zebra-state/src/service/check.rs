//! Consensus critical contextual checks

use std::{borrow::Borrow, sync::Arc};

use chrono::Duration;

use mset::MultiSet;

use zebra_chain::block::subsidy::funding_streams::funding_stream_address;
use zebra_chain::error::CoinbaseTransactionError;
use zebra_chain::parameters::subsidy::SubsidyError;
use zebra_chain::{
    amount::{
        Amount, DeferredPoolBalanceChange, Error as AmountError, NegativeAllowed, NonNegative,
    },
    block::{
        self, subsidy::new_coinbase_script, Block, ChainHistoryBlockTxAuthCommitmentHash,
        CommitmentError, Height,
    },
    history_tree::HistoryTree,
    parameters::{
        subsidy::{funding_stream_values, FundingStreamReceiver},
        Network, NetworkUpgrade,
    },
    transaction::{self, Transaction},
    transparent::Output,
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

#[cfg(not(zcash_unstable = "zip234"))]
use zebra_chain::parameters::subsidy::block_subsidy_pre_nsm;

#[cfg(zcash_unstable = "zip234")]
use zebra_chain::{amount::MAX_MONEY, parameters::subsidy::block_subsidy};

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
/// The semantically verified block is modified here to include any deferred pool balance change.
///
/// Additional contextual validity checks are performed by the non-finalized [`Chain`].
pub(crate) fn initial_contextual_validity(
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
    semantically_verified: &mut SemanticallyVerifiedBlock,
) -> Result<(), ValidateContextError> {
    let relevant_chain = any_ancestor_blocks(
        non_finalized_state,
        finalized_state,
        semantically_verified.block.header.previous_block_hash,
    );

    // [ZIP-234]: Block subsidy can be calculated only when the pool balances from previous block are known
    let network = &non_finalized_state.network;
    if semantically_verified.height > network.slow_start_interval() {
        #[cfg(not(zcash_unstable = "zip234"))]
        let expected_block_subsidy = block_subsidy_pre_nsm(semantically_verified.height, network)?;

        #[cfg(zcash_unstable = "zip234")]
        let expected_block_subsidy = {
            let pool_value_balance = non_finalized_state
                .best_chain()
                .map(|chain| chain.chain_value_pools)
                .or_else(|| {
                    finalized_state
                        .finalized_tip_height()
                        .filter(|x| (*x + 1).unwrap() == semantically_verified.height)
                        .map(|_| finalized_state.finalized_value_pool())
                });

            let money_reserve = if semantically_verified.height > 1.try_into().unwrap() {
                pool_value_balance
                    .expect("a chain must contain valid pool value balance")
                    .money_reserve()
            } else {
                MAX_MONEY.try_into().unwrap()
            };
            block_subsidy(semantically_verified.height, network, money_reserve)?
        };

        // See [ZIP-1015](https://zips.z.cash/zip-1015).
        semantically_verified.deferred_pool_balance_change = Some(subsidy_is_valid(
            &semantically_verified.block,
            network,
            expected_block_subsidy,
        )?);

        let coinbase_tx = coinbase_is_first(&semantically_verified.block)
            .map_err(|e| SubsidyError::Other(format!("invalid coinbase transaction: {e}")))?;

        // Blocks restored from disk may not have this field set. In that case, we treat
        // missing miner fees as zero fees.
        if let Some(block_miner_fees) = semantically_verified.block_miner_fees {
            miner_fees_are_valid(
                &coinbase_tx,
                semantically_verified.height,
                block_miner_fees,
                expected_block_subsidy,
                semantically_verified.deferred_pool_balance_change.unwrap(),
                network,
            )?;
        }
    }

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

/// Checks if there is exactly one coinbase transaction in `Block`,
/// and if that coinbase transaction is the first transaction in the block.
/// Returns the coinbase transaction is successful.
///
/// > A transaction that has a single transparent input with a null prevout field,
/// > is called a coinbase transaction. Every block has a single coinbase
/// > transaction as the first transaction in the block.
///
/// <https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions>
pub fn coinbase_is_first(
    block: &Block,
) -> Result<Arc<transaction::Transaction>, CoinbaseTransactionError> {
    // # Consensus
    //
    // > A block MUST have at least one transaction
    //
    // <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    let first = block
        .transactions
        .first()
        .ok_or(CoinbaseTransactionError::NoTransactions)?;
    // > The first transaction in a block MUST be a coinbase transaction,
    // > and subsequent transactions MUST NOT be coinbase transactions.
    //
    // <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    //
    // > A transaction that has a single transparent input with a null prevout
    // > field, is called a coinbase transaction.
    //
    // <https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions>
    let mut rest = block.transactions.iter().skip(1);
    if !first.is_coinbase() {
        Err(CoinbaseTransactionError::Position)?;
    }
    // > A transparent input in a non-coinbase transaction MUST NOT have a null prevout
    //
    // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
    if !rest.all(|tx| tx.is_valid_non_coinbase()) {
        Err(CoinbaseTransactionError::AfterFirst)?;
    }

    Ok(first.clone())
}

/// Returns `Ok()` with the deferred pool balance change of the coinbase transaction if
/// the block subsidy in `block` is valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(
    block: &Block,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<DeferredPoolBalanceChange, SubsidyError> {
    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;
    let coinbase = block.transactions.first().ok_or(SubsidyError::NoCoinbase)?;

    // Validate funding streams
    let Some(halving_div) = zebra_chain::parameters::subsidy::halving_divisor(height, network)
    else {
        // Far future halving, with no founders reward or funding streams
        return Ok(DeferredPoolBalanceChange::zero());
    };

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is known");

    let slow_start_interval = network.slow_start_interval();

    if height < slow_start_interval {
        unreachable!(
            "unsupported block height: callers should handle blocks below {:?}",
            slow_start_interval
        )
    } else if halving_div.count_ones() != 1 {
        unreachable!("invalid halving divisor: the halving divisor must be a non-zero power of two")
    } else if height < canopy_activation_height {
        // Founders rewards are paid up to Canopy activation, on both mainnet and testnet.
        // But we checkpoint in Canopy so founders reward does not apply for Zebra.
        unreachable!("we cannot verify consensus rules before Canopy activation");
    } else if halving_div < 8 {
        let mut coinbase_outputs: MultiSet<Output> = coinbase.outputs().iter().cloned().collect();

        // Funding streams are paid from Canopy activation to the second halving
        // Note: Canopy activation is at the first halving on mainnet, but not on testnet
        // ZIP-1014 only applies to mainnet, ZIP-214 contains the specific rules for testnet
        // funding stream amount values
        let mut funding_streams = funding_stream_values(height, network, expected_block_subsidy)
            .map_err(|err| SubsidyError::Other(err.to_string()))?;

        let mut has_expected_output = |address, expected_amount| {
            coinbase_outputs.remove(&Output::new_coinbase(
                expected_amount,
                new_coinbase_script(address),
            ))
        };

        // The deferred pool contribution is checked in `miner_fees_are_valid()`
        // See [ZIP-1015](https://zips.z.cash/zip-1015) for more details.
        let mut deferred_pool_balance_change = funding_streams
            .remove(&FundingStreamReceiver::Deferred)
            .unwrap_or_default()
            .constrain::<NegativeAllowed>()
            .map_err(|e| SubsidyError::Other(format!("invalid deferred pool amount: {e}")))?;

        // Checks the one-time lockbox disbursements in the NU6.1 activation block's coinbase transaction
        // See [ZIP-271](https://zips.z.cash/zip-0271) and [ZIP-1016](https://zips.z.cash/zip-1016) for more details.
        let expected_one_time_lockbox_disbursements = network.lockbox_disbursements(height);
        for (address, expected_amount) in &expected_one_time_lockbox_disbursements {
            if !has_expected_output(address, *expected_amount) {
                Err(SubsidyError::OneTimeLockboxDisbursementNotFound)?;
            }

            deferred_pool_balance_change = deferred_pool_balance_change
                .checked_sub(*expected_amount)
                .expect("should be a valid Amount");
        }

        // # Consensus
        //
        // > [Canopy onward] The coinbase transaction at block height `height`
        // > MUST contain at least one output per funding stream `fs` active at `height`,
        // > that pays `fs.Value(height)` zatoshi in the prescribed way to the stream's
        // > recipient address represented by `fs.AddressList[fs.AddressIndex(height)]
        //
        // https://zips.z.cash/protocol/protocol.pdf#fundingstreams
        for (receiver, expected_amount) in funding_streams {
            let address = funding_stream_address(height, network, receiver)
                // funding stream receivers other than the deferred pool must have an address
                .ok_or_else(|| {
                    SubsidyError::Other(format!(
                        "missing funding stream address at height {height:?}"
                    ))
                })?;

            if !has_expected_output(address, expected_amount) {
                Err(SubsidyError::FundingStreamNotFound)?;
            }
        }

        Ok(DeferredPoolBalanceChange::new(deferred_pool_balance_change))
    } else {
        // Future halving, with no founders reward or funding streams
        Ok(DeferredPoolBalanceChange::zero())
    }
}

/// Returns `Ok(())` if the miner fees consensus rule is valid.
///
/// [7.1.2]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
pub fn miner_fees_are_valid(
    coinbase_tx: &Transaction,
    height: Height,
    block_miner_fees: Amount<NonNegative>,
    expected_block_subsidy: Amount<NonNegative>,
    expected_deferred_pool_balance_change: DeferredPoolBalanceChange,
    network: &Network,
) -> Result<(), SubsidyError> {
    let transparent_value_balance = coinbase_tx
        .outputs()
        .iter()
        .map(|output| output.value())
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(|_| SubsidyError::SumOverflow)?
        .constrain()
        .map_err(|e| SubsidyError::Other(format!("invalid transparent value balance: {e}")))?;
    let sapling_value_balance = coinbase_tx.sapling_value_balance().sapling_amount();
    let orchard_value_balance = coinbase_tx.orchard_value_balance().orchard_amount();

    // Coinbase transaction can still have a NSM deposit
    let zip233_amount: Amount<NegativeAllowed> = coinbase_tx
        .zip233_amount()
        .constrain()
        .expect("positive value always fit in `NegativeAllowed`");

    // # Consensus
    //
    // > - define the total output value of its coinbase transaction to be the total value in zatoshi of its transparent
    // >   outputs, minus vbalanceSapling, minus vbalanceOrchard, plus totalDeferredOutput(height);
    // > – define the total input value of its coinbase transaction to be the value in zatoshi of the block subsidy,
    // >   plus the transaction fees paid by transactions in the block.
    //
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    //
    // The expected lockbox funding stream output of the coinbase transaction is also subtracted
    // from the block subsidy value plus the transaction fees paid by transactions in this block.
    let total_output_value =
        (transparent_value_balance - sapling_value_balance - orchard_value_balance
            + expected_deferred_pool_balance_change.value()
            + zip233_amount)
            .map_err(|_| SubsidyError::SumOverflow)?;

    let total_input_value =
        (expected_block_subsidy + block_miner_fees).map_err(|_| SubsidyError::SumOverflow)?;

    // # Consensus
    //
    // > [Pre-NU6] The total output of a coinbase transaction MUST NOT be greater than its total
    // input.
    //
    // > [NU6 onward] The total output of a coinbase transaction MUST be equal to its total input.
    if if NetworkUpgrade::current(network, height) < NetworkUpgrade::Nu6 {
        total_output_value > total_input_value
    } else {
        total_output_value != total_input_value
    } {
        Err(SubsidyError::InvalidMinerFees)?
    };

    #[cfg(zcash_unstable = "zip235")]
    if let Some(nsm_activation_height) = NetworkUpgrade::Nu7.activation_height(network) {
        if height >= nsm_activation_height {
            let minimum_zip233_amount = ((block_miner_fees * 6).unwrap() / 10).unwrap();
            if zip233_amount < minimum_zip233_amount {
                Err(SubsidyError::InvalidZip233Amount)?
            }
        }
    }

    Ok(())
}
