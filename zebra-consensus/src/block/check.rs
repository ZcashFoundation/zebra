//! Consensus check functions

use chrono::{DateTime, Utc};
use std::collections::HashSet;

use zebra_chain::{
    amount::{Amount, Error as AmountError, NonNegative},
    block::{Block, Hash, Header, Height},
    parameters::{Network, NetworkUpgrade},
    transaction,
    work::{difficulty::ExpandedDifficulty, equihash},
};

use crate::{error::*, parameters::SLOW_START_INTERVAL};

use super::subsidy;

/// Returns `Ok(())` if there is exactly one coinbase transaction in `Block`,
/// and that coinbase transaction is the first transaction in the block.
///
/// "The first (and only the first) transaction in a block is a coinbase
/// transaction, which collects and spends any miner subsidy and transaction
/// fees paid by transactions included in this block." [§3.10][3.10]
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub fn coinbase_is_first(block: &Block) -> Result<(), BlockError> {
    let first = block
        .transactions
        .get(0)
        .ok_or(BlockError::NoTransactions)?;
    let mut rest = block.transactions.iter().skip(1);
    if !first.has_valid_coinbase_transaction_inputs() {
        return Err(TransactionError::CoinbasePosition)?;
    }
    if rest.any(|tx| tx.has_any_coinbase_inputs()) {
        return Err(TransactionError::CoinbaseAfterFirst)?;
    }

    Ok(())
}

/// Returns `Ok(())` if `hash` passes:
///   - the target difficulty limit for `network` (PoWLimit), and
///   - the difficulty filter,
/// based on the fields in `header`.
///
/// If the block is invalid, returns an error containing `height` and `hash`.
pub fn difficulty_is_valid(
    header: &Header,
    network: Network,
    height: &Height,
    hash: &Hash,
) -> Result<(), BlockError> {
    let difficulty_threshold = header
        .difficulty_threshold
        .to_expanded()
        .ok_or(BlockError::InvalidDifficulty(*height, *hash))?;

    // Note: the comparisons in this function are u256 integer comparisons, like
    // zcashd and bitcoin. Greater values represent *less* work.

    // The PowLimit check is part of `Threshold()` in the spec, but it doesn't
    // actually depend on any previous blocks.
    if difficulty_threshold > ExpandedDifficulty::target_difficulty_limit(network) {
        Err(BlockError::TargetDifficultyLimit(
            *height,
            *hash,
            difficulty_threshold,
            network,
            ExpandedDifficulty::target_difficulty_limit(network),
        ))?;
    }

    // The difficulty filter is also context-free.
    //
    // ZIP 205 and ZIP 208 incorrectly describe testnet minimum difficulty blocks
    // as a change to the difficulty filter. But in `zcashd`, it is implemented
    // as a change to the difficulty adjustment algorithm. So we don't need to
    // do anything special for testnet here.
    // For details, see https://github.com/zcash/zips/issues/416
    if hash > &difficulty_threshold {
        Err(BlockError::DifficultyFilter(
            *height,
            *hash,
            difficulty_threshold,
            network,
        ))?;
    }

    Ok(())
}

/// Returns `Ok(())` if the `EquihashSolution` is valid for `header`
pub fn equihash_solution_is_valid(header: &Header) -> Result<(), equihash::Error> {
    header.solution.check(header)
}

/// Returns `Ok(())` if the block subsidy in `block` is valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(block: &Block, network: Network) -> Result<(), BlockError> {
    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;
    let coinbase = block.transactions.get(0).ok_or(SubsidyError::NoCoinbase)?;

    // Validate founders reward and funding streams
    let halving_div = subsidy::general::halving_divisor(height, network);
    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is known");

    if height < SLOW_START_INTERVAL {
        unreachable!(
            "unsupported block height: callers should handle blocks below {:?}",
            SLOW_START_INTERVAL
        )
    } else if halving_div.count_ones() != 1 {
        unreachable!("invalid halving divisor: the halving divisor must be a non-zero power of two")
    } else if height < canopy_activation_height {
        // Founders rewards are paid up to Canopy activation, on both mainnet and testnet
        let founders_reward = subsidy::founders_reward::founders_reward(height, network)
            .expect("invalid Amount: founders reward should be valid");
        let matching_values = subsidy::general::find_output_with_amount(coinbase, founders_reward);

        // TODO: the exact founders reward value must be sent as a single output to the correct address
        if !matching_values.is_empty() {
            Ok(())
        } else {
            Err(SubsidyError::FoundersRewardNotFound)?
        }
    } else if halving_div < 4 {
        // Funding streams are paid from Canopy activation to the second halving
        // Note: Canopy activation is at the first halving on mainnet, but not on testnet
        // ZIP-1014 only applies to mainnet, ZIP-214 contains the specific rules for testnet
        // funding stream amount values
        let funding_streams = subsidy::funding_streams::funding_stream_values(height, network)
            .expect("We always expect a funding stream hashmap response even if empty");

        // Consensus rule:[Canopy onward] The coinbase transaction at block height `height`
        // MUST contain at least one output per funding stream `fs` active at `height`,
        // that pays `fs.Value(height)` zatoshi in the prescribed way to the stream's
        // recipient address represented by `fs.AddressList[fs.AddressIndex(height)]
        for (receiver, expected_amount) in funding_streams {
            let address =
                subsidy::funding_streams::funding_stream_address(height, network, receiver);

            let has_expected_output =
                subsidy::funding_streams::filter_outputs_by_address(coinbase, address)
                    .iter()
                    .map(zebra_chain::transparent::Output::value)
                    .any(|value| value == expected_amount);

            if !has_expected_output {
                Err(SubsidyError::FundingStreamNotFound)?;
            }
        }
        Ok(())
    } else {
        // Future halving, with no founders reward or funding streams
        Ok(())
    }
}

/// Returns `Ok(())` if the miner fees consensus rule is valid.
///
/// [7.1.2]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
pub fn miner_fees_are_valid(
    block: &Block,
    network: Network,
    block_miner_fees: Amount<NonNegative>,
) -> Result<(), BlockError> {
    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;
    let coinbase = block.transactions.get(0).ok_or(SubsidyError::NoCoinbase)?;

    let transparent_value_balance: Amount = subsidy::general::output_amounts(coinbase)
        .iter()
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(|_| SubsidyError::SumOverflow)?
        .constrain()
        .expect("positive value always fit in `NegativeAllowed`");
    let sapling_value_balance = coinbase.sapling_value_balance().sapling_amount();
    let orchard_value_balance = coinbase.orchard_value_balance().orchard_amount();

    let block_subsidy = subsidy::general::block_subsidy(height, network)
        .expect("a valid block subsidy for this height and network");

    // Consensus rule: The total value in zatoshi of transparent outputs from a
    // coinbase transaction, minus vbalanceSapling, minus vbalanceOrchard, MUST NOT
    // be greater than the value in zatoshi of block subsidy plus the transaction fees
    // paid by transactions in this block.
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    let left = (transparent_value_balance - sapling_value_balance - orchard_value_balance)
        .map_err(|_| SubsidyError::SumOverflow)?;
    let right = (block_subsidy + block_miner_fees).map_err(|_| SubsidyError::SumOverflow)?;

    if left > right {
        return Err(SubsidyError::InvalidMinerFees)?;
    }

    Ok(())
}

/// Returns `Ok(())` if `header.time` is less than or equal to
/// 2 hours in the future, according to the node's local clock (`now`).
///
/// This is a non-deterministic rule, as clocks vary over time, and
/// between different nodes.
///
/// "In addition, a full validator MUST NOT accept blocks with nTime
/// more than two hours in the future according to its clock. This
/// is not strictly a consensus rule because it is nondeterministic,
/// and clock time varies between nodes. Also note that a block that
/// is rejected by this rule at a given point in time may later be
/// accepted." [§7.5][7.5]
///
/// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
///
/// If the header time is invalid, returns an error containing `height` and `hash`.
pub fn time_is_valid_at(
    header: &Header,
    now: DateTime<Utc>,
    height: &Height,
    hash: &Hash,
) -> Result<(), zebra_chain::block::BlockTimeError> {
    header.time_is_valid_at(now, height, hash)
}

/// Check Merkle root validity.
///
/// `transaction_hashes` is a precomputed list of transaction hashes.
///
/// # Consensus rules:
///
/// - The nConsensusBranchId field MUST match the consensus branch ID used for
///  SIGHASH transaction hashes, as specified in [ZIP-244] ([7.1]).
/// - A SHA-256d hash in internal byte order. The merkle root is derived from the
///  hashes of all transactions included in this block, ensuring that none of
///  those transactions can be modified without modifying the header. [7.6]
///
/// # Panics
///
/// - If block does not have a coinbase transaction.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
/// [7.1]: https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
/// [7.6]: https://zips.z.cash/protocol/nu5.pdf#blockheader
pub fn merkle_root_validity(
    network: Network,
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> Result<(), BlockError> {
    // TODO: deduplicate zebra-chain and zebra-consensus errors (#2908)
    block
        .check_transaction_network_upgrade_consistency(network)
        .map_err(|_| BlockError::WrongTransactionConsensusBranchId)?;

    let merkle_root = transaction_hashes.iter().cloned().collect();

    if block.header.merkle_root != merkle_root {
        return Err(BlockError::BadMerkleRoot {
            actual: merkle_root,
            expected: block.header.merkle_root,
        });
    }

    // Bitcoin's transaction Merkle trees are malleable, allowing blocks with
    // duplicate transactions to have the same Merkle root as blocks without
    // duplicate transactions.
    //
    // Collecting into a HashSet deduplicates, so this checks that there are no
    // duplicate transaction hashes, preventing Merkle root malleability.
    //
    // ## Full Block Validation
    //
    // Duplicate transactions should cause a block to be
    // rejected, as duplicate transactions imply that the block contains a
    // double-spend.  As a defense-in-depth, however, we also check that there
    // are no duplicate transaction hashes.
    //
    // ## Checkpoint Validation
    //
    // To prevent malleability (CVE-2012-2459), we also need to check
    // whether the transaction hashes are unique.
    if transaction_hashes.len() != transaction_hashes.iter().collect::<HashSet<_>>().len() {
        return Err(BlockError::DuplicateTransaction);
    }

    Ok(())
}

/// Returns `Ok(())` if the expiry height for the coinbase transaction is valid
/// according to specifications [7.1] and [ZIP-203].
///
/// [7.1]: https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
/// [ZIP-203]: https://zips.z.cash/zip-0203
pub fn coinbase_expiry_height(
    height: &Height,
    coinbase: &transaction::Transaction,
    network: Network,
) -> Result<(), BlockError> {
    match NetworkUpgrade::Nu5.activation_height(network) {
        // If Nu5 does not have a height, apply the pre-Nu5 rule.
        None => validate_expiry_height_max(coinbase.expiry_height()),
        Some(activation_height) => {
            // Conesnsus rule: from NU5 activation, the nExpiryHeight field of a
            // coinbase transaction MUST be set equal to the block height.
            if *height >= activation_height {
                match coinbase.expiry_height() {
                    None => Err(TransactionError::CoinbaseExpiration)?,
                    Some(expiry) => {
                        if expiry != *height {
                            return Err(TransactionError::CoinbaseExpiration)?;
                        }
                    }
                }
                return Ok(());
            }
            // Consensus rule: [Overwinter to Canopy inclusive, pre-NU5] nExpiryHeight
            // MUST be less than or equal to 499999999.
            validate_expiry_height_max(coinbase.expiry_height())
        }
    }
}

/// Validate the consensus rule: nExpiryHeight MUST be less than or equal to 499999999.
fn validate_expiry_height_max(expiry_height: Option<Height>) -> Result<(), BlockError> {
    if let Some(expiry) = expiry_height {
        if expiry > Height::MAX_EXPIRY_HEIGHT {
            return Err(TransactionError::CoinbaseExpiration)?;
        }
    }

    Ok(())
}
