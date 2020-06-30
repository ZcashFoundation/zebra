//! Module of types for working with validated zatoshi Amounts
use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
    ops::RangeInclusive,
};

/// A runtime validated type for representing amounts of zatoshis
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub struct Amount<C = NegativeAllowed>(i64, PhantomData<C>);

impl<C1, C2> std::ops::Add<Amount<C2>> for Amount<C1>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn add(self, rhs: Amount<C2>) -> Self::Output {
        let value = self.0 + rhs.0;
        value.try_into().ok()
    }
}

impl<C1, C2> std::ops::Add<Amount<C2>> for Option<Amount<C1>>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn add(self, rhs: Amount<C2>) -> Self::Output {
        self.and_then(|this| this + rhs)
    }
}

impl<C1, C2> std::ops::Add<Option<Amount<C2>>> for Amount<C1>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn add(self, rhs: Option<Amount<C2>>) -> Self::Output {
        rhs.and_then(|rhs| self + rhs)
    }
}

impl<C1, C2> std::ops::AddAssign<Amount<C2>> for Option<Amount<C1>>
where
    Option<Amount<C1>>: Copy,
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    fn add_assign(&mut self, rhs: Amount<C2>) {
        *self = self.and_then(|this| this + rhs);
    }
}

impl<C1, C2> std::ops::Sub<Amount<C2>> for Amount<C1>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn sub(self, rhs: Amount<C2>) -> Self::Output {
        let value = self.0 - rhs.0;
        value.try_into().ok()
    }
}

impl<C1, C2> std::ops::Sub<Amount<C2>> for Option<Amount<C1>>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn sub(self, rhs: Amount<C2>) -> Self::Output {
        self.and_then(|this| this - rhs)
    }
}

impl<C1, C2> std::ops::Sub<Option<Amount<C2>>> for Amount<C1>
where
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    type Output = Option<Amount<C1>>;

    fn sub(self, rhs: Option<Amount<C2>>) -> Self::Output {
        rhs.and_then(|rhs| self - rhs)
    }
}

impl<C1, C2> std::ops::SubAssign<Amount<C2>> for Option<Amount<C1>>
where
    Option<Amount<C1>>: Copy,
    C1: AmountConstraint,
    C2: AmountConstraint,
{
    fn sub_assign(&mut self, rhs: Amount<C2>) {
        *self = self.and_then(|this| this - rhs);
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

impl<C> TryFrom<i64> for Amount<C>
where
    C: AmountConstraint,
{
    type Error = Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

impl<C> TryFrom<i32> for Amount<C>
where
    C: AmountConstraint,
{
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        C::validate(value as _).map(|v| Self(v, PhantomData))
    }
}

impl<C> TryFrom<u64> for Amount<C>
where
    C: AmountConstraint,
{
    type Error = Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let value = value
            .try_into()
            .map_err(|source| Error::Convert { value, source })?;

        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

#[derive(thiserror::Error, Debug, displaydoc::Display)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Marker type for `Amount` that restricts the values to `-MAX_MONEY..=MAX_MONEY`
pub enum NegativeAllowed {}

impl AmountConstraint for NegativeAllowed {
    fn valid_range() -> RangeInclusive<i64> {
        -MAX_MONEY..=MAX_MONEY
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Marker type for `Amount` that restricts the value to positive numbers `0..=MAX_MONEY`
pub enum NonNegative {}

impl AmountConstraint for NonNegative {
    fn valid_range() -> RangeInclusive<i64> {
        0..=MAX_MONEY
    }
}

/// The max amount of money that can be obtained in zatoshis
pub const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

/// A trait for defining constraints on `Amount`
pub trait AmountConstraint {
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

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::eyre::Result;
    use proptest::prelude::*;
    use std::fmt;

    impl<C> Arbitrary for Amount<C>
    where
        C: AmountConstraint + fmt::Debug,
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
        let one: Amount = 1.try_into().unwrap();
        let neg_one: Amount = (-1).try_into().unwrap();

        let zero: Amount = 0.try_into().unwrap();
        let new_zero = one + neg_one;

        assert_eq!(Some(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_add_opt_lhs() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let one = Some(one);
        let neg_one: Amount = (-1).try_into().unwrap();

        let zero: Amount = 0.try_into().unwrap();
        let new_zero = one + neg_one;

        assert_eq!(Some(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_add_opt_rhs() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let neg_one: Amount = (-1).try_into().unwrap();
        let neg_one = Some(neg_one);

        let zero: Amount = 0.try_into().unwrap();
        let new_zero = one + neg_one;

        assert_eq!(Some(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_add_opt_both() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let one = Some(one);
        let neg_one: Amount = (-1).try_into().unwrap();
        let neg_one = Some(neg_one);

        let zero: Amount = 0.try_into().unwrap();
        let new_zero = one.and_then(|one| one + neg_one);

        assert_eq!(Some(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_add_assign() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let neg_one: Amount = (-1).try_into().unwrap();
        let mut neg_one = Some(neg_one);

        let zero: Amount = 0.try_into().unwrap();
        neg_one += one;
        let new_zero = neg_one;

        assert_eq!(Some(zero), new_zero);

        Ok(())
    }

    #[test]
    fn test_sub_bare() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let zero: Amount = 0.try_into().unwrap();

        let neg_one: Amount = (-1).try_into().unwrap();
        let new_neg_one = zero - one;

        assert_eq!(Some(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn test_sub_opt_lhs() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let one = Some(one);
        let zero: Amount = 0.try_into().unwrap();

        let neg_one: Amount = (-1).try_into().unwrap();
        let new_neg_one = zero - one;

        assert_eq!(Some(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn test_sub_opt_rhs() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let zero: Amount = 0.try_into().unwrap();
        let zero = Some(zero);

        let neg_one: Amount = (-1).try_into().unwrap();
        let new_neg_one = zero - one;

        assert_eq!(Some(neg_one), new_neg_one);

        Ok(())
    }

    #[test]
    fn test_sub_assign() -> Result<()> {
        zebra_test::init();
        let one: Amount = 1.try_into().unwrap();
        let zero: Amount = 0.try_into().unwrap();
        let mut zero = Some(zero);

        let neg_one: Amount = (-1).try_into().unwrap();
        zero -= one;
        let new_neg_one = zero;

        assert_eq!(Some(neg_one), new_neg_one);

        Ok(())
    }
}
