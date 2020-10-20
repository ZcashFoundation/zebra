//! Consensus check functions

use chrono::{DateTime, Utc};

use zebra_chain::{
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
    if !first.is_coinbase() {
        return Err(TransactionError::CoinbasePosition)?;
    }
    if rest.any(|tx| tx.contains_coinbase_input()) {
        return Err(TransactionError::CoinbaseInputFound)?;
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
    header.solution.check(&header)
}

/// Returns `Ok(())` if the block subsidy and miner fees in `block` are valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(block: &Block, network: Network) -> Result<(), BlockError> {
    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;
    let coinbase = block.transactions.get(0).ok_or(SubsidyError::NoCoinbase)?;

    let halving_div = subsidy::general::halving_divisor(height, network);

    // TODO: the sum of the coinbase transaction outputs must be less than or equal to the block subsidy plus transaction fees

    // Check founders reward and funding streams
    if height < SLOW_START_INTERVAL {
        unreachable!(
            "unsupported block height: callers should handle blocks below {:?}",
            SLOW_START_INTERVAL
        )
    } else if halving_div.count_ones() != 1 {
        unreachable!("invalid halving divisor: the halving divisor must be a non-zero power of two")
    } else if subsidy::founders_reward::founders_reward_active(height, network) {
        // Founders rewards are paid up to Canopy activation, on both mainnet and testnet
        let founders_reward = subsidy::founders_reward::founders_reward(height, network)
            .expect("invalid Amount: founders reward should be valid");
        let matching_values = subsidy::general::find_output_with_amount(coinbase, founders_reward);

        // TODO: no validation yet, just calling founders reward address functions
        let founders_reward_address =
            subsidy::founders_reward::founders_reward_address(height, network)
                .expect("we should have an address");
        let matching_address = subsidy::founders_reward::find_output_with_address(
            coinbase,
            founders_reward_address,
            network,
        );

        // TODO: the exact founders reward value must be sent as a single output to the correct address
        if !matching_values.is_empty()
            && !matching_address.is_empty()
            && matching_values == matching_address
        {
            Ok(())
        } else {
            Err(SubsidyError::FoundersRewardNotFound)?
        }
    } else if halving_div < 4 {
        // Funding streams are paid from Canopy activation to the second halving
        // Note: Canopy activation is at the first halving on mainnet, but not on testnet
        // ZIP-1014 only applies to mainnet, ZIP-214 contains the specific rules for testnet
        tracing::trace!("funding stream block subsidy validation is not implemented");
        // Return ok for now
        Ok(())
    } else {
        // Future halving, with no founders reward or funding streams
        Ok(())
    }
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
pub fn merkle_root_validity(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> Result<(), BlockError> {
    let merkle_root = transaction_hashes.iter().cloned().collect();

    if block.header.merkle_root != merkle_root {
        return Err(BlockError::BadMerkleRoot {
            actual: merkle_root,
            expected: block.header.merkle_root,
        });
    }

    // Bitcoin's transaction Merkle trees are malleable, allowing blocks with
    // duplicate transactions to have the same Merkle root as blocks without
    // duplicate transactions. Duplicate transactions should cause a block to be
    // rejected, as duplicate transactions imply that the block contains a
    // double-spend.  As a defense-in-depth, however, we also check that there
    // are no duplicate transaction hashes, by collecting into a HashSet.
    use std::collections::HashSet;
    if transaction_hashes.len() != transaction_hashes.iter().collect::<HashSet<_>>().len() {
        return Err(BlockError::DuplicateTransaction);
    }

    Ok(())
}
