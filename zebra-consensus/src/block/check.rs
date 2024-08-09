//! Consensus check functions

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, Error as AmountError, NonNegative},
    block::{Block, Hash, Header, Height},
    parameters::{subsidy::FundingStreamReceiver, Network, NetworkUpgrade},
    transaction::{self, Transaction},
    work::{
        difficulty::{ExpandedDifficulty, ParameterDifficulty as _},
        equihash,
    },
};

use crate::error::*;

use super::subsidy;

/// Checks if there is exactly one coinbase transaction in `Block`,
/// and if that coinbase transaction is the first transaction in the block.
/// Returns the coinbase transaction is successful.
///
/// > A transaction that has a single transparent input with a null prevout field,
/// > is called a coinbase transaction. Every block has a single coinbase
/// > transaction as the first transaction in the block.
///
/// <https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions>
pub fn coinbase_is_first(block: &Block) -> Result<Arc<transaction::Transaction>, BlockError> {
    // # Consensus
    //
    // > A block MUST have at least one transaction
    //
    // <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    let first = block
        .transactions
        .first()
        .ok_or(BlockError::NoTransactions)?;
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
        Err(TransactionError::CoinbasePosition)?;
    }
    // > A transparent input in a non-coinbase transaction MUST NOT have a null prevout
    //
    // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
    if !rest.all(|tx| tx.is_valid_non_coinbase()) {
        Err(TransactionError::CoinbaseAfterFirst)?;
    }

    Ok(first.clone())
}

/// Returns `Ok(ExpandedDifficulty)` if the`difficulty_threshold` of `header` is at least as difficult as
/// the target difficulty limit for `network` (PoWLimit)
///
/// If the header difficulty threshold is invalid, returns an error containing `height` and `hash`.
pub fn difficulty_threshold_is_valid(
    header: &Header,
    network: &Network,
    height: &Height,
    hash: &Hash,
) -> Result<ExpandedDifficulty, BlockError> {
    let difficulty_threshold = header
        .difficulty_threshold
        .to_expanded()
        .ok_or(BlockError::InvalidDifficulty(*height, *hash))?;

    // Note: the comparison in this function is a u256 integer comparison, like
    // zcashd and bitcoin. Greater values represent *less* work.

    // The PowLimit check is part of `Threshold()` in the spec, but it doesn't
    // actually depend on any previous blocks.
    if difficulty_threshold > network.target_difficulty_limit() {
        Err(BlockError::TargetDifficultyLimit(
            *height,
            *hash,
            difficulty_threshold,
            network.clone(),
            network.target_difficulty_limit(),
        ))?;
    }

    Ok(difficulty_threshold)
}

/// Returns `Ok(())` if `hash` passes:
///   - the target difficulty limit for `network` (PoWLimit), and
///   - the difficulty filter,
///
/// based on the fields in `header`.
///
/// If the block is invalid, returns an error containing `height` and `hash`.
pub fn difficulty_is_valid(
    header: &Header,
    network: &Network,
    height: &Height,
    hash: &Hash,
) -> Result<(), BlockError> {
    let difficulty_threshold = difficulty_threshold_is_valid(header, network, height, hash)?;

    // Note: the comparison in this function is a u256 integer comparison, like
    // zcashd and bitcoin. Greater values represent *less* work.

    // # Consensus
    //
    // > The block MUST pass the difficulty filter.
    //
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    //
    // The difficulty filter is also context-free.
    if hash > &difficulty_threshold {
        Err(BlockError::DifficultyFilter(
            *height,
            *hash,
            difficulty_threshold,
            network.clone(),
        ))?;
    }

    Ok(())
}

/// Returns `Ok(())` if the `EquihashSolution` is valid for `header`
pub fn equihash_solution_is_valid(header: &Header) -> Result<(), equihash::Error> {
    // # Consensus
    //
    // > `solution` MUST represent a valid Equihash solution.
    //
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    header.solution.check(header)
}

/// Returns `Ok(())` if the block subsidy in `block` is valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(
    block: &Block,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<(), BlockError> {
    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;
    let coinbase = block.transactions.first().ok_or(SubsidyError::NoCoinbase)?;

    // Validate funding streams
    let Some(halving_div) = subsidy::general::halving_divisor(height, network) else {
        // Far future halving, with no founders reward or funding streams
        return Ok(());
    };

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is known");

    // TODO: Add this as a field on `testnet::Parameters` instead of checking `disable_pow()`, this is 0 for Regtest in zcashd,
    //       see <https://github.com/zcash/zcash/blob/master/src/chainparams.cpp#L640>
    let slow_start_interval = if network.disable_pow() {
        Height(0)
    } else {
        network.slow_start_interval()
    };

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
        // Funding streams are paid from Canopy activation to the second halving
        // Note: Canopy activation is at the first halving on mainnet, but not on testnet
        // ZIP-1014 only applies to mainnet, ZIP-214 contains the specific rules for testnet
        // funding stream amount values
        let funding_streams = subsidy::funding_streams::funding_stream_values(
            height,
            network,
            expected_block_subsidy,
        )
        .expect("We always expect a funding stream hashmap response even if empty");

        // # Consensus
        //
        // > [Canopy onward] The coinbase transaction at block height `height`
        // > MUST contain at least one output per funding stream `fs` active at `height`,
        // > that pays `fs.Value(height)` zatoshi in the prescribed way to the stream's
        // > recipient address represented by `fs.AddressList[fs.AddressIndex(height)]
        //
        // https://zips.z.cash/protocol/protocol.pdf#fundingstreams
        for (receiver, expected_amount) in funding_streams {
            if receiver == FundingStreamReceiver::Deferred {
                // The deferred pool contribution is checked in `miner_fees_are_valid()`
                // TODO: Add link to lockbox stream ZIP
                continue;
            }

            let address = subsidy::funding_streams::funding_stream_address(
                height, network, receiver,
            )
            .expect("funding stream receivers other than the deferred pool must have an address");

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
    coinbase_tx: &Transaction,
    height: Height,
    block_miner_fees: Amount<NonNegative>,
    expected_block_subsidy: Amount<NonNegative>,
    expected_deferred_amount: Amount<NonNegative>,
    network: &Network,
) -> Result<(), BlockError> {
    let transparent_value_balance = subsidy::general::output_amounts(coinbase_tx)
        .iter()
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(|_| SubsidyError::SumOverflow)?
        .constrain()
        .expect("positive value always fit in `NegativeAllowed`");
    let sapling_value_balance = coinbase_tx.sapling_value_balance().sapling_amount();
    let orchard_value_balance = coinbase_tx.orchard_value_balance().orchard_amount();

    // TODO: Update the quote below once its been updated for NU6.
    //
    // # Consensus
    //
    // > The total value in zatoshi of transparent outputs from a coinbase transaction,
    // > minus vbalanceSapling, minus vbalanceOrchard, MUST NOT be greater than the value
    // > in zatoshi of block subsidy plus the transaction fees paid by transactions in this block.
    //
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    //
    // The expected lockbox funding stream output of the coinbase transaction is also subtracted
    // from the block subsidy value plus the transaction fees paid by transactions in this block.
    let left = (transparent_value_balance - sapling_value_balance - orchard_value_balance)
        .map_err(|_| SubsidyError::SumOverflow)?;
    let right = (expected_block_subsidy + block_miner_fees - expected_deferred_amount)
        .map_err(|_| SubsidyError::SumOverflow)?;

    // TODO: Updadte the quotes below if the final phrasing changes in the spec for NU6.
    //
    // # Consensus
    //
    // > [Pre-NU6] The total output of a coinbase transaction MUST NOT be greater than its total
    // input.
    //
    // > [NU6 onward] The total output of a coinbase transaction MUST be equal to its total input.
    if if NetworkUpgrade::current(network, height) < NetworkUpgrade::Nu6 {
        left > right
    } else {
        left != right
    } {
        Err(SubsidyError::InvalidMinerFees)?
    };

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
/// - A SHA-256d hash in internal byte order. The merkle root is derived from the
///   hashes of all transactions included in this block, ensuring that none of
///   those transactions can be modified without modifying the header. [7.6]
///
/// # Panics
///
/// - If block does not have a coinbase transaction.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
/// [7.1]: https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
/// [7.6]: https://zips.z.cash/protocol/nu5.pdf#blockheader
pub fn merkle_root_validity(
    network: &Network,
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
    // double-spend. As a defense-in-depth, however, we also check that there
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
