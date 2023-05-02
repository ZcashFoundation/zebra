//! ZIP-317 tests.

use super::{mempool_checks, Amount, Error};
#[test]
fn zip317_unpaid_actions_err() {
    let check = mempool_checks(51, Amount::try_from(1).unwrap(), 1);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::UnpaidActions));
}

#[test]
fn zip317_minimum_rate_fee_err() {
    let check = mempool_checks(50, Amount::try_from(1).unwrap(), 1000);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::FeeBelowMinimumRate));
}
