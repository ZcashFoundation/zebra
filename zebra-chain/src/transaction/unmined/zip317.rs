//! The [ZIP-317 conventional fee calculation](https://zips.z.cash/zip-0317#fee-calculation)
//! for [UnminedTx]s.

use crate::{
    amount::{Amount, NonNegative},
    transaction::Transaction,
};

// For doc links
#[allow(unused_imports)]
use crate::transaction::UnminedTx;

/// Returns the conventional fee for `transaction`, as defined by [ZIP-317].
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#fee-calculation
pub fn conventional_fee(transaction: &Transaction) -> Amount<NonNegative> {
    todo!()
}
