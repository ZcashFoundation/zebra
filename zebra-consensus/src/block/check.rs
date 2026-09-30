//! Consensus check functions

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{Block, Hash, Header, Height},
    parameters::{
        subsidy::{self, CoinbaseTransactionError},
        Network,
    },
    transaction::{self, Transaction},
    work::{
        difficulty::{ExpandedDifficulty, ParameterDifficulty as _},
        equihash,
    },
};

use crate::error::*;

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

/// Returns `Ok()` with the deferred pool balance change of the coinbase transaction if the block
/// subsidy in `block` is valid for `network`
///
/// See [`zebra_chain::parameters::subsidy::subsidy_is_valid`].
pub fn subsidy_is_valid(
    block: &Block,
    net: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<DeferredPoolBalanceChange, BlockError> {
    subsidy::subsidy_is_valid(block, net, expected_block_subsidy).map_err(subsidy_block_error)
}

/// Returns `Ok(())` if the miner fees consensus rule is valid.
///
/// See [`zebra_chain::parameters::subsidy::miner_fees_are_valid`].
pub fn miner_fees_are_valid(
    coinbase_tx: &Transaction,
    height: Height,
    block_miner_fees: Amount<NonNegative>,
    expected_block_subsidy: Amount<NonNegative>,
    expected_deferred_pool_balance_change: DeferredPoolBalanceChange,
    network: &Network,
) -> Result<(), BlockError> {
    subsidy::miner_fees_are_valid(
        coinbase_tx,
        height,
        block_miner_fees,
        expected_block_subsidy,
        expected_deferred_pool_balance_change,
        network,
    )
    .map_err(subsidy_block_error)
}

/// Converts a [`CoinbaseTransactionError`] from the subsidy checks into the [`BlockError`] the
/// block verifier reports for it.
fn subsidy_block_error(error: CoinbaseTransactionError) -> BlockError {
    match error {
        CoinbaseTransactionError::Subsidy(error) => error.into(),
        error => BlockError::Other(error.to_string()),
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
