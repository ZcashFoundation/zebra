//! An implementation of the [ZIP-317] fee calculations for [UnminedTx]s:
//! - [conventional fee](https://zips.z.cash/zip-0317#fee-calculation)
//! - [block production transaction weight](https://zips.z.cash/zip-0317#block-production)

use std::cmp::max;

use crate::{
    amount::{Amount, NonNegative},
    serialization::ZcashSerialize,
    transaction::{Transaction, UnminedTx},
};

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

/// The recommended weight cap for ZIP-317 block production.
const MAX_BLOCK_PRODUCTION_WEIGHT: f32 = 4.0;

/// Zebra's custom minimum weight for ZIP-317 block production,
/// based on half the [ZIP-203] recommended transaction expiry height of 40 blocks.
///
/// This ensures all transactions have a non-zero probability of being mined,
/// which simplifies our implementation.
///
/// If blocks are full, this makes it likely that very low fee transactions
/// will be mined:
/// - after approximately 20 blocks delay,
/// - but before they expire.
///
/// Note: Small transactions that pay the legacy ZIP-313 conventional fee have twice this weight.
/// If blocks are full, they will be mined after approximately 10 blocks delay.
///
/// [ZIP-203]: https://zips.z.cash/zip-0203#changes-for-blossom>
const MIN_BLOCK_PRODUCTION_WEIGHT: f32 = 1.0 / 20.0;

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

/// Returns the block production fee weight for `transaction`, as defined by [ZIP-317].
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub fn block_production_fee_weight(transaction: &UnminedTx, miner_fee: Amount<NonNegative>) -> f32 {
    let miner_fee = i64::from(miner_fee) as f32;
    let conventional_fee = i64::from(transaction.conventional_fee) as f32;

    let uncapped_weight = miner_fee / conventional_fee;

    uncapped_weight.clamp(MIN_BLOCK_PRODUCTION_WEIGHT, MAX_BLOCK_PRODUCTION_WEIGHT)
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
