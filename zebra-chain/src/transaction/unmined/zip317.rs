//! The [ZIP-317 conventional fee calculation](https://zips.z.cash/zip-0317#fee-calculation)
//! for [UnminedTx]s.

use crate::{
    amount::{Amount, NonNegative},
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
    todo!()
}
