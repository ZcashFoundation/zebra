//! Fixed test vectors for amounts.

use crate::serialization::ZcashDeserializeInto;

use super::super::*;

use std::{collections::hash_map::RandomState, collections::HashSet, fmt::Debug};

use color_eyre::eyre::Result;

#[test]
fn test_add_bare() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let neg_one: Amount = (-1).try_into()?;

    let zero: Amount = Amount::zero();
    let new_zero = one + neg_one;

    assert_eq!(zero, new_zero?);

    Ok(())
}

#[test]
fn test_add_opt_lhs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let one = Ok(one);
    let neg_one: Amount = (-1).try_into()?;

    let zero: Amount = Amount::zero();
    let new_zero = one + neg_one;

    assert_eq!(zero, new_zero?);

    Ok(())
}

#[test]
fn test_add_opt_rhs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let neg_one: Amount = (-1).try_into()?;
    let neg_one = Ok(neg_one);

    let zero: Amount = Amount::zero();
    let new_zero = one + neg_one;

    assert_eq!(zero, new_zero?);

    Ok(())
}

#[test]
fn test_add_opt_both() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let one = Ok(one);
    let neg_one: Amount = (-1).try_into()?;
    let neg_one = Ok(neg_one);

    let zero: Amount = Amount::zero();
    let new_zero = one.and_then(|one| one + neg_one);

    assert_eq!(zero, new_zero?);

    Ok(())
}

#[test]
fn test_add_assign() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let neg_one: Amount = (-1).try_into()?;
    let mut neg_one = Ok(neg_one);

    let zero: Amount = Amount::zero();
    neg_one += one;
    let new_zero = neg_one;

    assert_eq!(Ok(zero), new_zero);

    Ok(())
}

#[test]
fn test_sub_bare() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let zero: Amount = Amount::zero();

    let neg_one: Amount = (-1).try_into()?;
    let new_neg_one = zero - one;

    assert_eq!(Ok(neg_one), new_neg_one);

    Ok(())
}

#[test]
fn test_sub_opt_lhs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let one = Ok(one);
    let zero: Amount = Amount::zero();

    let neg_one: Amount = (-1).try_into()?;
    let new_neg_one = zero - one;

    assert_eq!(Ok(neg_one), new_neg_one);

    Ok(())
}

#[test]
fn test_sub_opt_rhs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let zero: Amount = Amount::zero();
    let zero = Ok(zero);

    let neg_one: Amount = (-1).try_into()?;
    let new_neg_one = zero - one;

    assert_eq!(Ok(neg_one), new_neg_one);

    Ok(())
}

#[test]
fn test_sub_assign() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let zero: Amount = Amount::zero();
    let mut zero = Ok(zero);

    let neg_one: Amount = (-1).try_into()?;
    zero -= one;
    let new_neg_one = zero;

    assert_eq!(Ok(neg_one), new_neg_one);

    Ok(())
}

#[test]
fn add_with_diff_constraints() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one = Amount::<NonNegative>::try_from(1)?;
    let zero: Amount<NegativeAllowed> = Amount::zero();

    (zero - one.constrain()).expect("should allow negative");
    (zero.constrain() - one).expect_err("shouldn't allow negative");

    Ok(())
}

#[test]
// The borrows are actually needed to call the correct trait impl
#[allow(clippy::needless_borrow)]
fn deserialize_checks_bounds() -> Result<()> {
    let _init_guard = zebra_test::init();

    let big = (MAX_MONEY * 2)
        .try_into()
        .expect("unexpectedly large constant: multiplied constant should be within range");
    let neg = -10;

    let mut big_bytes = Vec::new();
    (&mut big_bytes)
        .write_u64::<LittleEndian>(big)
        .expect("unexpected serialization failure: vec should be infallible");

    let mut neg_bytes = Vec::new();
    (&mut neg_bytes)
        .write_i64::<LittleEndian>(neg)
        .expect("unexpected serialization failure: vec should be infallible");

    Amount::<NonNegative>::zcash_deserialize(big_bytes.as_slice())
        .expect_err("deserialization should reject too large values");
    Amount::<NegativeAllowed>::zcash_deserialize(big_bytes.as_slice())
        .expect_err("deserialization should reject too large values");

    Amount::<NonNegative>::zcash_deserialize(neg_bytes.as_slice())
        .expect_err("NonNegative deserialization should reject negative values");
    let amount: Amount<NegativeAllowed> = neg_bytes
        .zcash_deserialize_into()
        .expect("NegativeAllowed deserialization should allow negative values");

    assert_eq!(amount.0, neg);

    Ok(())
}

#[test]
fn hash() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one = Amount::<NonNegative>::try_from(1)?;
    let another_one = Amount::<NonNegative>::try_from(1)?;
    let zero: Amount<NonNegative> = Amount::zero();

    let hash_set: HashSet<Amount<NonNegative>, RandomState> = [one].iter().cloned().collect();
    assert_eq!(hash_set.len(), 1);

    let hash_set: HashSet<Amount<NonNegative>, RandomState> = [one, one].iter().cloned().collect();
    assert_eq!(hash_set.len(), 1, "Amount hashes are consistent");

    let hash_set: HashSet<Amount<NonNegative>, RandomState> =
        [one, another_one].iter().cloned().collect();
    assert_eq!(hash_set.len(), 1, "Amount hashes are by value");

    let hash_set: HashSet<Amount<NonNegative>, RandomState> = [one, zero].iter().cloned().collect();
    assert_eq!(
        hash_set.len(),
        2,
        "Amount hashes are different for different values"
    );

    Ok(())
}

#[test]
fn ordering_constraints() -> Result<()> {
    let _init_guard = zebra_test::init();

    ordering::<NonNegative, NonNegative>()?;
    ordering::<NonNegative, NegativeAllowed>()?;
    ordering::<NegativeAllowed, NonNegative>()?;
    ordering::<NegativeAllowed, NegativeAllowed>()?;

    Ok(())
}

#[allow(clippy::eq_op)]
fn ordering<C1, C2>() -> Result<()>
where
    C1: Constraint + Debug,
    C2: Constraint + Debug,
{
    let zero: Amount<C1> = Amount::zero();
    let one = Amount::<C2>::try_from(1)?;
    let another_one = Amount::<C1>::try_from(1)?;

    assert_eq!(one, one);
    assert_eq!(one, another_one, "Amount equality is by value");

    assert_ne!(one, zero);
    assert_ne!(zero, one);

    assert!(one > zero);
    assert!(zero < one);
    assert!(zero <= one);

    let negative_one = Amount::<NegativeAllowed>::try_from(-1)?;
    let negative_two = Amount::<NegativeAllowed>::try_from(-2)?;

    assert_ne!(negative_one, zero);
    assert_ne!(negative_one, one);

    assert!(negative_one < zero);
    assert!(negative_one <= one);
    assert!(zero > negative_one);
    assert!(zero >= negative_one);
    assert!(negative_two < negative_one);
    assert!(negative_one > negative_two);

    Ok(())
}

#[test]
fn test_sum() -> Result<()> {
    let _init_guard = zebra_test::init();

    let one: Amount = 1.try_into()?;
    let neg_one: Amount = (-1).try_into()?;

    let zero: Amount = Amount::zero();

    // success
    let amounts = vec![one, neg_one, zero];

    let sum_ref: Amount = amounts.iter().sum::<Result<Amount, Error>>()?;
    let sum_value: Amount = amounts.into_iter().sum::<Result<Amount, Error>>()?;

    assert_eq!(sum_ref, sum_value);
    assert_eq!(sum_ref, zero);

    // above max for Amount error
    let max: Amount = MAX_MONEY.try_into()?;
    let amounts = vec![one, max];
    let integer_sum: i64 = amounts.iter().map(|a| a.0).sum();

    let sum_ref = amounts.iter().sum::<Result<Amount, Error>>();
    let sum_value = amounts.into_iter().sum::<Result<Amount, Error>>();

    assert_eq!(sum_ref, sum_value);
    assert_eq!(
        sum_ref,
        Err(Error::SumOverflow {
            partial_sum: integer_sum,
            remaining_items: 0
        })
    );

    // below min for Amount error
    let min: Amount = (-MAX_MONEY).try_into()?;
    let amounts = vec![min, neg_one];
    let integer_sum: i64 = amounts.iter().map(|a| a.0).sum();

    let sum_ref = amounts.iter().sum::<Result<Amount, Error>>();
    let sum_value = amounts.into_iter().sum::<Result<Amount, Error>>();

    assert_eq!(sum_ref, sum_value);
    assert_eq!(
        sum_ref,
        Err(Error::SumOverflow {
            partial_sum: integer_sum,
            remaining_items: 0
        })
    );

    // above max of i64 error
    let times: usize = (i64::MAX / MAX_MONEY)
        .try_into()
        .expect("4392 can always be converted to usize");
    let amounts: Vec<Amount> = std::iter::repeat_n(MAX_MONEY.try_into()?, times + 1).collect();

    let sum_ref = amounts.iter().sum::<Result<Amount, Error>>();
    let sum_value = amounts.into_iter().sum::<Result<Amount, Error>>();

    assert_eq!(sum_ref, sum_value);
    assert_eq!(
        sum_ref,
        Err(Error::SumOverflow {
            partial_sum: 4200000000000000,
            remaining_items: 4391
        })
    );

    // below min of i64 overflow
    let times: usize = (i64::MAX / MAX_MONEY)
        .try_into()
        .expect("4392 can always be converted to usize");
    let neg_max_money: Amount<NegativeAllowed> = (-MAX_MONEY).try_into()?;
    let amounts: Vec<Amount<NegativeAllowed>> =
        std::iter::repeat_n(neg_max_money, times + 1).collect();

    let sum_ref = amounts.iter().sum::<Result<Amount, Error>>();
    let sum_value = amounts.into_iter().sum::<Result<Amount, Error>>();

    assert_eq!(sum_ref, sum_value);
    assert_eq!(
        sum_ref,
        Err(Error::SumOverflow {
            partial_sum: -4200000000000000,
            remaining_items: 4391
        })
    );

    Ok(())
}
