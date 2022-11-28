//! The [ZIP-317 conventional fee calculation](https://zips.z.cash/zip-0317#fee-calculation)
//! for [UnminedTx]s.

use std::cmp::max;

use crate::{
    amount::{Amount, NonNegative},
    serialization::ZcashSerialize,
    transaction::Transaction,
};

// For doc links
#[allow(unused_imports)]
use crate::transaction::UnminedTx;

/// The marginal fee for the ZIP-317 fee calculation, in zatoshis per logical action.
//
// TODO: allow Amount<NonNegative> in constants
const MARGINAL_FEE: i64 = 5_000;

/// The number of grace logical actions allowed by the ZIP-317 fee calculation.
const GRACE_ACTIONS: u64 = 2;

/// The standard size of p2pkh inputs for the ZIP-317 fee calculation, in bytes.
const P2PKH_STANDARD_INPUT_SIZE: usize = 150;

/// The standard size of p2pkh outputs for the ZIP-317 fee calculation, in bytes.
const P2PKH_STANDARD_OUTPUT_SIZE: usize = 34;

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
    let n_actions_orchard = transaction.orchard_actions().count();

    let tx_in_logical_actions = div_ceil(tx_in_total_size, P2PKH_STANDARD_INPUT_SIZE);
    let tx_out_logical_actions = div_ceil(tx_out_total_size, P2PKH_STANDARD_OUTPUT_SIZE);

    let logical_actions = max(tx_in_logical_actions, tx_out_logical_actions)
        + 2 * n_join_split
        + max(n_spends_sapling, n_outputs_sapling)
        + n_actions_orchard;
    let logical_actions: u64 = logical_actions
        .try_into()
        .expect("transaction items are limited by serialized size limit");

    let conventional_fee = marginal_fee * max(GRACE_ACTIONS, logical_actions);

    conventional_fee.expect("conventional fee is positive and limited by serialized size limit")
}

/// Divide `quotient` by `divisor`, rounding the result up to the nearest integer.
///
/// # Correctness
///
/// `quotient + divisor` must be less than `usize::MAX`.
/// `divisor` must not be zero.
//
// TODO: replace with usize::div_ceil() when int_roundings stabilises:
// https://github.com/rust-lang/rust/issues/88581
fn div_ceil(quotient: usize, divisor: usize) -> usize {
    // Rust uses truncated integer division, so this is equivalent to:
    // `ceil(quotient/divisor)`
    // as long as the addition doesn't overflow or underflow.
    (quotient + divisor - 1) / divisor
}
