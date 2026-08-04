//! Fixed test vectors for value balances.

use crate::{
    amount::{Amount, NegativeAllowed, NonNegative, MAX_MONEY},
    value_balance::{ValueBalance, ValueBalanceError},
};

/// Check that the ironwood pool (NU6.3) participates in the ZIP-209 chain value
/// pool non-negativity rule, exactly like the other pools.
#[test]
fn ironwood_pool_enforces_non_negative_balance() {
    let _init_guard = zebra_test::init();

    // Adding positive ironwood value to an empty pool is valid, and is tracked
    // in the ironwood pool.
    let chain = ValueBalance::<NonNegative>::zero()
        .add_chain_value_pool_change(ValueBalance::from_ironwood_amount(
            Amount::<NegativeAllowed>::try_from(100).expect("valid amount"),
        ))
        .expect("adding positive ironwood value to an empty pool is valid");

    assert_eq!(
        chain.ironwood_amount(),
        Amount::<NonNegative>::try_from(100).expect("valid amount"),
    );

    // Draining more ironwood value than the pool holds must be rejected with an
    // ironwood-specific error, exactly like the sapling/orchard pools (ZIP-209).
    let error = chain
        .add_chain_value_pool_change(ValueBalance::from_ironwood_amount(
            Amount::<NegativeAllowed>::try_from(-101).expect("valid amount"),
        ))
        .expect_err("draining the ironwood pool below zero must be rejected");

    assert!(matches!(error, ValueBalanceError::Ironwood(_)));
}

/// Check that `add_chain_value_pool_change` rejects a chain value pool whose
/// individual pools are each within the valid `Amount` range, but whose total
/// exceeds `MAX_MONEY` (the total monetary base cap).
#[test]
fn total_over_max_money_is_rejected() {
    let _init_guard = zebra_test::init();

    // Start from a pool that already holds the maximum value in the transparent
    // pool. This is individually valid (`transparent` is within `0..=MAX_MONEY`).
    let mut chain = ValueBalance::<NonNegative>::zero();
    chain.set_transparent_value_balance(ValueBalance::from_transparent_amount(
        Amount::try_from(MAX_MONEY).expect("MAX_MONEY is a valid amount"),
    ));

    // Add the maximum value to the sprout pool. Each pool remains individually
    // valid (`sprout` is within `0..=MAX_MONEY`), but the total becomes
    // `2 * MAX_MONEY`, which exceeds the `MAX_MONEY` cap on the monetary base.
    let error = chain
        .add_chain_value_pool_change(ValueBalance::from_sprout_amount(
            Amount::<NegativeAllowed>::try_from(MAX_MONEY).expect("MAX_MONEY is a valid amount"),
        ))
        .expect_err("a total exceeding MAX_MONEY must be rejected");

    assert!(matches!(error, ValueBalanceError::Total(_)));
}

/// Check that the ironwood value balance is included in a transaction's
/// remaining value.
#[test]
fn ironwood_included_in_remaining_transaction_value() {
    let _init_guard = zebra_test::init();

    let value_balance =
        ValueBalance::<NegativeAllowed>::from_ironwood_amount(Amount::try_from(50).expect("valid"));

    assert_eq!(
        value_balance
            .remaining_transaction_value()
            .expect("positive ironwood value is valid remaining transaction value"),
        Amount::<NonNegative>::try_from(50).expect("valid amount"),
    );
}
