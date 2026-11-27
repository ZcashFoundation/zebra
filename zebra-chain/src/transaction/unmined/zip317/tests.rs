//! ZIP-317 tests.

use super::{mempool_checks, Amount, Error, GRACE_ACTIONS, MARGINAL_FEE};
use crate::block::MAX_BLOCK_BYTES;

#[test]
fn zip317_unpaid_actions_err() {
    let check = mempool_checks(1, Amount::try_from(1).unwrap(), 1);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::UnpaidActions));
}

#[test]
fn zip317_minimum_rate_fee_err() {
    let check = mempool_checks(0, Amount::try_from(1).unwrap(), 1000);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::FeeBelowMinimumRate));
}

#[test]
fn zip317_mempool_checks_ok() {
    assert!(mempool_checks(0, Amount::try_from(100).unwrap(), 1000).is_ok())
}

#[test]
fn minimum_conventional_fee_passes_the_size_floor_at_any_size() {
    let min_conventional_fee = Amount::try_from(u64::from(GRACE_ACTIONS) * MARGINAL_FEE)
        .expect("the minimum conventional fee is far below the maximum amount");
    let max_block_size = usize::try_from(MAX_BLOCK_BYTES)
        .expect("the block size limit is 2 MB, which fits in usize");

    for transaction_size in [1, 1_000, 8_000, 8_010, 10_000, 100_000, max_block_size] {
        assert_eq!(
            mempool_checks(0, min_conventional_fee, transaction_size),
            Ok(()),
            "a transaction of {transaction_size} bytes that pays the minimum conventional fee \
             must pass the size-based minimum fee",
        );
    }
}
