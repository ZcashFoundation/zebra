//! Strongly-typed zatoshi amounts that prevent under/overflows.
//!
//! The [`Amount`] type is parameterized by a [`Constraint`] implementation that
//! declares the range of allowed values. In contrast to regular arithmetic
//! operations, which return values, arithmetic on [`Amount`]s returns
//! [`Result`](std::result::Result)s.

use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
    ops::RangeInclusive,
};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};

type Result<T, E = Error> = std::result::Result<T, E>;

/// A runtime validated type for representing amounts of zatoshis
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "i64")]
#[serde(bound = "C: Constraint")]
pub struct Amount<C = NegativeAllowed>(i64, PhantomData<C>);

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
pub struct NegativeAllowed {}

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonNegative {}

impl Constraint for NonNegative {
    fn valid_range() -> RangeInclusive<i64> {
        0..=MAX_MONEY
    }
}

/// The maximum zatoshi amount.
pub const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

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

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::eyre::Result;
    use proptest::prelude::*;
    use std::fmt;

    impl<C> Arbitrary for Amount<C>
    where
        C: Constraint + fmt::Debug,
    {
        type Parameters = ();

        fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
            C::valid_range().prop_map(|v| Self(v, PhantomData)).boxed()
        }

        type Strategy = BoxedStrategy<Self>;
    }

    #[test]
    fn test_add_bare() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into()?;
        let neg_one: Amount = (-1).try_into()?;

        let zero: Amount = 0.try_into()?;
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

        let zero: Amount = 0.try_into()?;
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

        let zero: Amount = 0.try_into()?;
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

        let zero: Amount = 0.try_into()?;
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

        let zero: Amount = 0.try_into()?;
        neg_one += one;
        let new_zero = neg_one;

        assert_eq!(Ok(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_sub_bare() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into()?;
        let zero: Amount = 0.try_into()?;

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
        let zero: Amount = 0.try_into()?;

        let neg_one: Amount = (-1).try_into()?;
        let new_neg_one = zero - one;

        assert_eq!(Ok(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn test_sub_opt_rhs() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into()?;
        let zero: Amount = 0.try_into()?;
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
        let zero: Amount = 0.try_into()?;
        let mut zero = Ok(zero);

        let neg_one: Amount = (-1).try_into()?;
        zero -= one;
        let new_neg_one = zero;

        assert_eq!(Ok(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn add_with_diff_constraints() -> Result<()> {
        let one = Amount::<NonNegative>::try_from(1)?;
        let zero = Amount::<NegativeAllowed>::try_from(0)?;

        (zero - one.constrain()).expect("should allow negative");
        (zero.constrain() - one).expect_err("shouldn't allow negative");

        Ok(())
    }

    #[test]
    fn deserialize_checks_bounds() -> Result<()> {
        let big = MAX_MONEY * 2;
        let neg = -10;

        let big_bytes = bincode::serialize(&big)?;
        let neg_bytes = bincode::serialize(&neg)?;

        bincode::deserialize::<Amount<NonNegative>>(&big_bytes)
            .expect_err("deserialization should reject too large values");
        bincode::deserialize::<Amount<NegativeAllowed>>(&big_bytes)
            .expect_err("deserialization should reject too large values");

        bincode::deserialize::<Amount<NonNegative>>(&neg_bytes)
            .expect_err("NonNegative deserialization should reject negative values");
        let amount = bincode::deserialize::<Amount<NegativeAllowed>>(&neg_bytes)
            .expect("NegativeAllowed deserialization should allow negative values");

        assert_eq!(amount.0, neg);

        Ok(())
    }
}
