//! ZIP-317 tests.

use super::{mempool_checks, Amount, Error};
#[test]
fn zip317_unpaid_actions_err() {
    let check = mempool_checks(
        51,
        50,
        Amount::try_from(1).unwrap(),
        Amount::try_from(10000).unwrap(),
    );

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::UnpaidActions));
}

#[test]
fn zip317_miner_fee_err() {
    let check = mempool_checks(
        50,
        50,
        Amount::try_from(1).unwrap(),
        Amount::try_from(10000).unwrap(),
    );

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::MinerFee));
}
