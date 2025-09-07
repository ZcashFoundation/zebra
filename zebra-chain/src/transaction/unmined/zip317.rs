//! An implementation of the [ZIP-317] fee calculations for [UnminedTx]s:
//! - [conventional fee](https://zips.z.cash/zip-0317#fee-calculation)
//! - [block production transaction weight](https://zips.z.cash/zip-0317#block-production)

use std::cmp::max;

use num_integer::div_ceil;
use thiserror::Error;

use crate::{
    amount::{Amount, NonNegative},
    block::MAX_BLOCK_BYTES,
    serialization::ZcashSerialize,
    transaction::{Transaction, UnminedTx},
};

#[cfg(test)]
mod tests;

/// The marginal fee for the ZIP-317 fee calculation, in zatoshis per logical action.
//
// TODO: allow Amount<NonNegative> in constants
const MARGINAL_FEE: u64 = 5_000;

/// The number of grace logical actions allowed by the ZIP-317 fee calculation.
const GRACE_ACTIONS: u32 = 2;

/// The standard size of p2pkh inputs for the ZIP-317 fee calculation, in bytes.
const P2PKH_STANDARD_INPUT_SIZE: usize = 150;

/// The standard size of p2pkh outputs for the ZIP-317 fee calculation, in bytes.
const P2PKH_STANDARD_OUTPUT_SIZE: usize = 34;

/// The recommended weight ratio cap for ZIP-317 block production.
/// `weight_ratio_cap` in ZIP-317.
const BLOCK_PRODUCTION_WEIGHT_RATIO_CAP: f32 = 4.0;

/// The minimum fee for the block production weight ratio calculation, in zatoshis.
/// If a transaction has a lower fee, this value is used instead.
///
/// This avoids special handling for transactions with zero weight.
const MIN_BLOCK_PRODUCTION_SUBSTITUTE_FEE: i64 = 1;

/// If a tx has more than `BLOCK_UNPAID_ACTION_LIMIT` "unpaid actions", it will never be mined by
/// the [_Recommended algorithm for block template construction_][alg-def], implemented in Zebra
/// [here][alg-impl].
///
/// [alg-def]: https://zips.z.cash/zip-0317#recommended-algorithm-for-block-template-construction
/// [alg-impl]: https://github.com/zcashfoundation/zebra/blob/95e4d0973caac075b47589f6a05f9d744acd3db3/zebra-rpc/src/methods/get_block_template_rpcs/zip317.rs#L39
pub const BLOCK_UNPAID_ACTION_LIMIT: u32 = 0;

/// The minimum fee per kilobyte for Zebra mempool transactions.
/// Also used as the minimum fee for a mempool transaction.
///
/// Based on `DEFAULT_MIN_RELAY_TX_FEE` in `zcashd`:
/// <https://github.com/zcash/zcash/blob/f512291ff20098291442e83713de89bcddc07546/src/main.h#L71-L72>
///
/// This is a `usize` to simplify transaction size-based calculation code.
pub const MIN_MEMPOOL_TX_FEE_RATE: usize = 100;

/// The fee cap for [`MIN_MEMPOOL_TX_FEE_RATE`] minimum required mempool fees.
///
/// Based on `LEGACY_DEFAULT_FEE` in `zcashd`:
/// <https://github.com/zcash/zcash/blob/9e856cfc5b81aa2607a16a23ff5584ea10014de6/src/amount.h#L35-L36>
///
/// This is a `usize` to simplify transaction size-based calculation code.
pub const MEMPOOL_TX_FEE_REQUIREMENT_CAP: usize = 1000;

/// Returns the conventional fee for `transaction`, as defined by [ZIP-317].
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#fee-calculation
pub fn conventional_fee(transaction: &Transaction) -> Amount<NonNegative> {
    // zcash_primitives checks for non-p2pkh inputs, but Zebra doesn't.
    // Conventional fees are only used in the standard rules for mempool eviction
    // and block production, so these implementations are compatible.
    //
    // <https://github.com/zcash/librustzcash/blob/main/zcash_primitives/src/transaction/fees/zip317.rs#L135>

    let marginal_fee: Amount<NonNegative> = MARGINAL_FEE.try_into().expect("fits in amount");

    // marginal_fee * max(logical_actions, GRACE_ACTIONS)
    let conventional_fee = marginal_fee * conventional_actions(transaction).into();

    conventional_fee.expect("conventional fee is positive and limited by serialized size limit")
}

/// Returns the number of unpaid actions for `transaction`, as defined by [ZIP-317].
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub fn unpaid_actions(transaction: &UnminedTx, miner_fee: Amount<NonNegative>) -> u32 {
    // max(logical_actions, GRACE_ACTIONS)
    let conventional_actions = conventional_actions(&transaction.transaction);

    // floor(tx.fee / marginal_fee)
    let marginal_fee_weight_ratio = miner_fee / MARGINAL_FEE;
    let marginal_fee_weight_ratio: i64 = marginal_fee_weight_ratio
        .expect("marginal fee is not zero")
        .into();

    // max(0, conventional_actions - marginal_fee_weight_ratio)
    //
    // Subtracting MAX_MONEY/5000 from a u32 can't go above i64::MAX.
    let unpaid_actions = i64::from(conventional_actions) - marginal_fee_weight_ratio;

    unpaid_actions.try_into().unwrap_or_default()
}

/// Returns the block production fee weight ratio for `transaction`, as defined by [ZIP-317].
///
/// This calculation will always return a positive, non-zero value.
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub fn conventional_fee_weight_ratio(
    transaction: &UnminedTx,
    miner_fee: Amount<NonNegative>,
) -> f32 {
    // Check that this function will always return a positive, non-zero value.
    //
    // The maximum number of logical actions in a block is actually
    // MAX_BLOCK_BYTES / MIN_ACTION_BYTES. MIN_ACTION_BYTES is currently
    // the minimum transparent output size, but future transaction versions could change this.
    assert!(
        MIN_BLOCK_PRODUCTION_SUBSTITUTE_FEE as f32 / MAX_BLOCK_BYTES as f32 > 0.0,
        "invalid block production constants: the minimum fee ratio must not be zero"
    );

    let miner_fee = max(miner_fee.into(), MIN_BLOCK_PRODUCTION_SUBSTITUTE_FEE) as f32;

    let conventional_fee = i64::from(transaction.conventional_fee) as f32;

    let uncapped_weight = miner_fee / conventional_fee;

    uncapped_weight.min(BLOCK_PRODUCTION_WEIGHT_RATIO_CAP)
}

/// Returns the conventional actions for `transaction`, `max(logical_actions, GRACE_ACTIONS)`,
/// as defined by [ZIP-317].
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#fee-calculation
pub fn conventional_actions(transaction: &Transaction) -> u32 {
    let tx_in_total_size: usize = transaction
        .inputs()
        .iter()
        .map(|input| input.zcash_serialized_size())
        .sum();

    let tx_out_total_size: usize = transaction
        .outputs()
        .iter()
        .map(|output| output.zcash_serialized_size())
        .sum();

    let n_join_split = transaction.joinsplit_count();
    let n_spends_sapling = transaction.sapling_spends_per_anchor().count();
    let n_outputs_sapling = transaction.sapling_outputs().count();
    let n_actions_orchard = transaction.orchard_action_count();

    let tx_in_logical_actions = div_ceil(tx_in_total_size, P2PKH_STANDARD_INPUT_SIZE);
    let tx_out_logical_actions = div_ceil(tx_out_total_size, P2PKH_STANDARD_OUTPUT_SIZE);

    let logical_actions = max(tx_in_logical_actions, tx_out_logical_actions)
        + 2 * n_join_split
        + max(n_spends_sapling, n_outputs_sapling)
        + n_actions_orchard;
    let logical_actions: u32 = logical_actions
        .try_into()
        .expect("transaction items are limited by serialized size limit");

    max(GRACE_ACTIONS, logical_actions)
}

/// Make ZIP-317 checks before inserting a transaction into the mempool.
pub fn mempool_checks(
    unpaid_actions: u32,
    miner_fee: Amount<NonNegative>,
    transaction_size: usize,
) -> Result<(), Error> {
    // # Standard Rule
    //
    // > If a transaction has more than `block_unpaid_action_limit` "unpaid actions" as defined by the
    // > Recommended algorithm for block template construction, it will never be mined by that algorithm.
    // > Nodes MAY drop these transactions.
    //
    // <https://zips.z.cash/zip-0317#transaction-relaying>
    if unpaid_actions > BLOCK_UNPAID_ACTION_LIMIT {
        return Err(Error::UnpaidActions);
    }

    // # Standard Rule
    //
    // > Nodes that normally relay transactions are expected to do so for transactions that pay at least the
    // > conventional fee as specified in this ZIP.
    //
    // <https://zips.z.cash/zip-0317#transaction-relaying>
    //
    // In Zebra, we use a similar minimum fee rate to `zcashd` v5.5.0 and later.
    // Transactions must pay a fee of at least 100 zatoshis per 1000 bytes of serialized size,
    // with a maximum fee of 1000 zatoshis.
    //
    // <https://github.com/zcash/zcash/blob/9e856cfc5b81aa2607a16a23ff5584ea10014de6/src/amount.cpp#L24-L37>
    //
    // In zcashd this is `DEFAULT_MIN_RELAY_TX_FEE` and `LEGACY_DEFAULT_FEE`:
    // <https://github.com/zcash/zcash/blob/f512291ff20098291442e83713de89bcddc07546/src/main.h#L71-L72>
    // <https://github.com/zcash/zcash/blob/9e856cfc5b81aa2607a16a23ff5584ea10014de6/src/amount.h#L35-L36>
    //
    // ## Note
    //
    // If the check above for the maximum number of unpaid actions passes with
    // [`BLOCK_UNPAID_ACTION_LIMIT`] set to zero, then there is no way for the legacy check below to
    // fail. This renders the legacy check redundant in that case.

    const KILOBYTE: usize = 1000;

    // This calculation can't overflow, because transactions are limited to 2 MB,
    // and usize is at least 4 GB.
    assert!(
        MIN_MEMPOOL_TX_FEE_RATE
            < usize::MAX / usize::try_from(MAX_BLOCK_BYTES).expect("constant fits in usize"),
        "the fee rate multiplication must never overflow",
    );

    let min_fee = (MIN_MEMPOOL_TX_FEE_RATE * transaction_size / KILOBYTE)
        .clamp(MIN_MEMPOOL_TX_FEE_RATE, MEMPOOL_TX_FEE_REQUIREMENT_CAP);
    let min_fee: u64 = min_fee
        .try_into()
        .expect("clamped value always fits in u64");
    let min_fee: Amount<NonNegative> = min_fee.try_into().expect("clamped value is positive");

    if miner_fee < min_fee {
        return Err(Error::FeeBelowMinimumRate);
    }

    Ok(())
}

/// Errors related to ZIP-317.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Error {
    #[error("Unpaid actions is higher than the limit")]
    UnpaidActions,

    #[error("Transaction fee is below the minimum fee rate")]
    FeeBelowMinimumRate,
}
