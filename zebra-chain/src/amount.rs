//! Strongly-typed zatoshi amounts that prevent under/overflows.
//!
//! The [`Amount`] type is parameterized by a [`Constraint`] implementation that
//! declares the range of allowed values. In contrast to regular arithmetic
//! operations, which return values, arithmetic on [`Amount`]s returns
//! [`Result`](std::result::Result)s.

use std::{
    cmp::Ordering,
    convert::{TryFrom, TryInto},
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::RangeInclusive,
};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A runtime validated type for representing amounts of zatoshis
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "i64")]
#[serde(bound = "C: Constraint")]
pub struct Amount<C = NegativeAllowed>(i64, PhantomData<C>);

impl<C> std::fmt::Debug for Amount<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple(&format!("Amount<{}>", std::any::type_name::<C>()))
            .field(&self.0)
            .finish()
    }
}

impl<C> Amount<C> {
    /// Convert this amount to a different Amount type if it satisfies the new constraint
    pub fn constrain<C2>(self) -> Result<Amount<C2>>
    where
        C2: Constraint,
    {
        self.0.try_into()
    }

    /// To little endian byte array
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut buf: [u8; 8] = [0; 8];
        LittleEndian::write_i64(&mut buf, self.0);
        buf
    }

    /// Create a zero `Amount`
    pub fn zero() -> Amount<C>
    where
        C: Constraint,
    {
        0.try_into().expect("an amount of 0 is always valid")
    }
}

impl<C> std::ops::Add<Amount<C>> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn add(self, rhs: Amount<C>) -> Self::Output {
        let value = self.0 + rhs.0;
        value.try_into()
    }
}

impl<C> std::ops::Add<Amount<C>> for Result<Amount<C>>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn add(self, rhs: Amount<C>) -> Self::Output {
        self? + rhs
    }
}

impl<C> std::ops::Add<Result<Amount<C>>> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn add(self, rhs: Result<Amount<C>>) -> Self::Output {
        self + rhs?
    }
}

impl<C> std::ops::AddAssign<Amount<C>> for Result<Amount<C>>
where
    Amount<C>: Copy,
    C: Constraint,
{
    fn add_assign(&mut self, rhs: Amount<C>) {
        if let Ok(lhs) = *self {
            *self = lhs + rhs;
        }
    }
}

impl<C> std::ops::Sub<Amount<C>> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn sub(self, rhs: Amount<C>) -> Self::Output {
        let value = self.0 - rhs.0;
        value.try_into()
    }
}

impl<C> std::ops::Sub<Amount<C>> for Result<Amount<C>>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn sub(self, rhs: Amount<C>) -> Self::Output {
        self? - rhs
    }
}

impl<C> std::ops::Sub<Result<Amount<C>>> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn sub(self, rhs: Result<Amount<C>>) -> Self::Output {
        self - rhs?
    }
}

impl<C> std::ops::SubAssign<Amount<C>> for Result<Amount<C>>
where
    Amount<C>: Copy,
    C: Constraint,
{
    fn sub_assign(&mut self, rhs: Amount<C>) {
        if let Ok(lhs) = *self {
            *self = lhs - rhs;
        }
    }
}

impl<C> From<Amount<C>> for i64 {
    fn from(amount: Amount<C>) -> Self {
        amount.0
    }
}

impl From<Amount<NonNegative>> for u64 {
    fn from(amount: Amount<NonNegative>) -> Self {
        amount.0 as _
    }
}

impl<C> From<Amount<C>> for jubjub::Fr {
    fn from(a: Amount<C>) -> jubjub::Fr {
        // TODO: this isn't constant time -- does that matter?
        if a.0 < 0 {
            jubjub::Fr::from(a.0.abs() as u64).neg()
        } else {
            jubjub::Fr::from(a.0 as u64)
        }
    }
}

impl<C> From<Amount<C>> for halo2::pasta::pallas::Scalar {
    fn from(a: Amount<C>) -> halo2::pasta::pallas::Scalar {
        // TODO: this isn't constant time -- does that matter?
        if a.0 < 0 {
            halo2::pasta::pallas::Scalar::from(a.0.abs() as u64).neg()
        } else {
            halo2::pasta::pallas::Scalar::from(a.0 as u64)
        }
    }
}

impl<C> TryFrom<i64> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

impl<C> TryFrom<i32> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        C::validate(value as _).map(|v| Self(v, PhantomData))
    }
}

impl<C> TryFrom<u64> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let value = value
            .try_into()
            .map_err(|source| Error::Convert { value, source })?;

        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

impl<C> Hash for Amount<C> {
    /// Amounts with the same value are equal, even if they have different constraints
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<C1, C2> PartialEq<Amount<C2>> for Amount<C1> {
    fn eq(&self, other: &Amount<C2>) -> bool {
        self.0.eq(&other.0)
    }
}

impl<C> PartialEq<i64> for Amount<C> {
    fn eq(&self, other: &i64) -> bool {
        self.0.eq(other)
    }
}

impl<C> PartialEq<Amount<C>> for i64 {
    fn eq(&self, other: &Amount<C>) -> bool {
        self.eq(&other.0)
    }
}

impl Eq for Amount<NegativeAllowed> {}
impl Eq for Amount<NonNegative> {}

impl<C1, C2> PartialOrd<Amount<C2>> for Amount<C1> {
    fn partial_cmp(&self, other: &Amount<C2>) -> Option<Ordering> {
        Some(self.0.cmp(&other.0))
    }
}

impl Ord for Amount<NegativeAllowed> {
    fn cmp(&self, other: &Amount<NegativeAllowed>) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Ord for Amount<NonNegative> {
    fn cmp(&self, other: &Amount<NonNegative>) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl std::ops::Mul<u64> for Amount<NonNegative> {
    type Output = Result<Amount<NonNegative>>;

    fn mul(self, rhs: u64) -> Self::Output {
        let value = (self.0 as u64)
            .checked_mul(rhs)
            .ok_or(Error::MultiplicationOverflow {
                amount: self.0,
                multiplier: rhs,
            })?;
        value.try_into()
    }
}

impl std::ops::Mul<Amount<NonNegative>> for u64 {
    type Output = Result<Amount<NonNegative>>;

    fn mul(self, rhs: Amount<NonNegative>) -> Self::Output {
        rhs.mul(self)
    }
}

impl std::ops::Div<u64> for Amount<NonNegative> {
    type Output = Result<Amount<NonNegative>>;

    fn div(self, rhs: u64) -> Self::Output {
        let quotient = (self.0 as u64)
            .checked_div(rhs)
            .ok_or(Error::DivideByZero { amount: self.0 })?;
        Ok(quotient
            .try_into()
            .expect("division by a positive integer always stays within the constraint"))
    }
}

impl<C> std::iter::Sum<Amount<C>> for Result<Amount<C>>
where
    C: Constraint,
{
    fn sum<I: Iterator<Item = Amount<C>>>(iter: I) -> Self {
        let mut overflow = false;
        let sum = iter
            .map(|a| a.0)
            .fold(0i64, |acc, amount| match acc.checked_add(amount) {
                Some(result) => result,
                None => {
                    overflow = true;
                    0
                }
            });
        if overflow {
            return Err(Error::SumOverflow);
        }

        Amount::try_from(sum)
    }
}

impl<'amt, C> std::iter::Sum<&'amt Amount<C>> for Result<Amount<C>>
where
    C: Constraint + std::marker::Copy + 'amt,
{
    fn sum<I: Iterator<Item = &'amt Amount<C>>>(iter: I) -> Self {
        iter.copied().sum()
    }
}

#[derive(thiserror::Error, Debug, displaydoc::Display, Clone, PartialEq)]
#[allow(missing_docs)]
/// Errors that can be returned when validating `Amount`s
pub enum Error {
    /// input {value} is outside of valid range for zatoshi Amount, valid_range={range:?}
    Contains {
        range: RangeInclusive<i64>,
        value: i64,
    },
    /// u64 {value} could not be converted to an i64 Amount
    Convert {
        value: u64,
        source: std::num::TryFromIntError,
    },
    /// i64 overflow when multiplying i64 non-negative amount {amount} by u64 {multiplier}
    MultiplicationOverflow { amount: i64, multiplier: u64 },
    /// cannot divide amount {amount} by zero
    DivideByZero { amount: i64 },

    /// Attempt to sum with overflow
    SumOverflow,
}

/// Marker type for `Amount` that allows negative values.
///
/// ```
/// # use zebra_chain::amount::{Constraint, MAX_MONEY, NegativeAllowed};
/// assert_eq!(
///     NegativeAllowed::valid_range(),
///     -MAX_MONEY..=MAX_MONEY,
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegativeAllowed;

impl Constraint for NegativeAllowed {
    fn valid_range() -> RangeInclusive<i64> {
        -MAX_MONEY..=MAX_MONEY
    }
}

/// Marker type for `Amount` that requires nonnegative values.
///
/// ```
/// # use zebra_chain::amount::{Constraint, MAX_MONEY, NonNegative};
/// assert_eq!(
///     NonNegative::valid_range(),
///     0..=MAX_MONEY,
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct NonNegative;

impl Constraint for NonNegative {
    fn valid_range() -> RangeInclusive<i64> {
        0..=MAX_MONEY
    }
}

/// Number of zatoshis in 1 ZEC
pub const COIN: i64 = 100_000_000;

/// The maximum zatoshi amount.
pub const MAX_MONEY: i64 = 21_000_000 * COIN;

/// A trait for defining constraints on `Amount`
pub trait Constraint {
    /// Returns the range of values that are valid under this constraint
    fn valid_range() -> RangeInclusive<i64>;

    /// Check if an input value is within the valid range
    fn validate(value: i64) -> Result<i64, Error> {
        let range = Self::valid_range();

        if !range.contains(&value) {
            Err(Error::Contains { range, value })
        } else {
            Ok(value)
        }
    }
}

impl ZcashSerialize for Amount<NegativeAllowed> {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_i64::<LittleEndian>(self.0)
    }
}

impl ZcashDeserialize for Amount<NegativeAllowed> {
    fn zcash_deserialize<R: std::io::Read>(
        mut reader: R,
    ) -> Result<Self, crate::serialization::SerializationError> {
        Ok(reader.read_i64::<LittleEndian>()?.try_into()?)
    }
}

impl ZcashSerialize for Amount<NonNegative> {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        let amount = self
            .0
            .try_into()
            .expect("constraint guarantees value is positive");

        writer.write_u64::<LittleEndian>(amount)
    }
}

impl ZcashDeserialize for Amount<NonNegative> {
    fn zcash_deserialize<R: std::io::Read>(
        mut reader: R,
    ) -> Result<Self, crate::serialization::SerializationError> {
        Ok(reader.read_u64::<LittleEndian>()?.try_into()?)
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::prelude::*;
#[cfg(any(test, feature = "proptest-impl"))]
impl<C> Arbitrary for Amount<C>
where
    C: Constraint + std::fmt::Debug,
{
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        C::valid_range().prop_map(|v| Self(v, PhantomData)).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod test {
    use crate::serialization::ZcashDeserializeInto;

    use super::*;

    use std::{collections::hash_map::RandomState, collections::HashSet, fmt::Debug};

    use color_eyre::eyre::Result;

    #[test]
    fn test_add_bare() -> Result<()> {
        zebra_test::init();

        let one: Amount = 1.try_into()?;
        let neg_one: Amount = (-1).try_into()?;

        let zero: Amount = Amount::zero();
        let new_zero = one + neg_one;

        assert_eq!(zero, new_zero?);

        Ok(())
    }

    #[test]
    fn test_add_opt_lhs() -> Result<()> {
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

        let one: Amount = 1.try_into()?;
        let zero: Amount = Amount::zero();

        let neg_one: Amount = (-1).try_into()?;
        let new_neg_one = zero - one;

        assert_eq!(Ok(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn test_sub_opt_lhs() -> Result<()> {
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

        let one = Amount::<NonNegative>::try_from(1)?;
        let zero: Amount<NegativeAllowed> = Amount::zero();

        (zero - one.constrain()).expect("should allow negative");
        (zero.constrain() - one).expect_err("shouldn't allow negative");

        Ok(())
    }

    #[test]
    fn deserialize_checks_bounds() -> Result<()> {
        zebra_test::init();

        let big = (MAX_MONEY * 2)
            .try_into()
            .expect("unexpectedly large constant: multiplied constant should be within range");
        let neg = -10;

        let mut big_bytes = Vec::new();
        (&mut big_bytes)
            .write_u64::<LittleEndian>(big)
            .expect("unexpected serialization failure: vec should be infalliable");

        let mut neg_bytes = Vec::new();
        (&mut neg_bytes)
            .write_i64::<LittleEndian>(neg)
            .expect("unexpected serialization failure: vec should be infalliable");

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
        zebra_test::init();

        let one = Amount::<NonNegative>::try_from(1)?;
        let another_one = Amount::<NonNegative>::try_from(1)?;
        let zero: Amount<NonNegative> = Amount::zero();

        let hash_set: HashSet<Amount<NonNegative>, RandomState> = [one].iter().cloned().collect();
        assert_eq!(hash_set.len(), 1);

        let hash_set: HashSet<Amount<NonNegative>, RandomState> =
            [one, one].iter().cloned().collect();
        assert_eq!(hash_set.len(), 1, "Amount hashes are consistent");

        let hash_set: HashSet<Amount<NonNegative>, RandomState> =
            [one, another_one].iter().cloned().collect();
        assert_eq!(hash_set.len(), 1, "Amount hashes are by value");

        let hash_set: HashSet<Amount<NonNegative>, RandomState> =
            [one, zero].iter().cloned().collect();
        assert_eq!(
            hash_set.len(),
            2,
            "Amount hashes are different for different values"
        );

        Ok(())
    }

    #[test]
    fn ordering_constraints() -> Result<()> {
        zebra_test::init();

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
        zebra_test::init();

        let one: Amount = 1.try_into()?;
        let neg_one: Amount = (-1).try_into()?;

        let zero: Amount = Amount::zero();

        // success
        let amounts = vec![one, neg_one, zero];
        // use iter to test reference-based sum
        let sum: Amount = amounts.iter().sum::<Result<Amount, Error>>()?;
        assert_eq!(sum, zero);

        // above max for Amount error
        let max: Amount = MAX_MONEY.try_into()?;
        let amounts = vec![one, max];
        let integer_sum: i64 = amounts.iter().map(|a| a.0).sum();

        // use into_iter to test value-based sum
        let err = match amounts.into_iter().sum::<Result<Amount, Error>>() {
            Err(e) => e,
            _ => unreachable!("above operation will always fail"),
        };
        assert_eq!(
            err,
            Error::Contains {
                range: -MAX_MONEY..=MAX_MONEY,
                value: integer_sum
            }
        );

        // below min for Amount error
        let min: Amount = (-MAX_MONEY).try_into()?;
        let amounts = vec![min, neg_one];
        let integer_sum: i64 = amounts.iter().map(|a| a.0).sum();

        // also use iter/copied to test value-based sum
        let err = match amounts.iter().copied().sum::<Result<Amount, Error>>() {
            Err(e) => e,
            _ => unreachable!("above operation will always fail"),
        };
        assert_eq!(
            err,
            Error::Contains {
                range: -MAX_MONEY..=MAX_MONEY,
                value: integer_sum
            }
        );

        // above max of i64 error
        let times = i64::MAX / MAX_MONEY;
        let mut amounts: Vec<Amount<NonNegative>> = vec![MAX_MONEY.try_into()?];
        for _ in 0..times {
            amounts.push(MAX_MONEY.try_into()?);
        }

        // use iter to test reference-based sum
        let err = match amounts.iter().sum() {
            Err(e) => e,
            _ => unreachable!("above operation will always fail"),
        };
        assert_eq!(err, Error::SumOverflow);

        // below min of i64 overflow
        let mut amounts: Vec<Amount<NegativeAllowed>> = vec![(-MAX_MONEY).try_into()?];
        for _ in 0..times {
            amounts.push((-MAX_MONEY).try_into()?);
        }

        // use into_iter to test value-based sum
        let err = match amounts.into_iter().sum() {
            Err(e) => e,
            _ => unreachable!("above operation will always fail"),
        };
        assert_eq!(err, Error::SumOverflow);

        Ok(())
    }
}
