//!
use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
    ops::Range,
};

/// A runtime validated type for representing amounts of zatoshis
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub struct Amount<C = NegativeAllowed>(i64, PhantomData<C>);

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

impl<C> TryFrom<u64> for Amount<C>
where
    C: AmountConstraint,
{
    type Error = Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let value = value.try_into()?;

        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

#[cfg(test)]
mod test {
    use super::*;
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
}

#[derive(thiserror::Error, Debug)]
#[allow(missing_docs)]
pub enum Error {
    #[error("input {value} is outside of the valid range {range:?}")]
    Contains { range: Range<i64>, value: i64 },
    #[error(transparent)]
    Truncate {
        #[from]
        source: std::num::TryFromIntError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
///
pub struct NegativeAllowed;

impl AmountConstraint for NegativeAllowed {
    fn valid_range() -> Range<i64> {
        -MAX_MONEY..MAX_MONEY + 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
///
pub struct NonNegative;

impl AmountConstraint for NonNegative {
    fn valid_range() -> Range<i64> {
        0..MAX_MONEY + 1
    }
}

const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

///
pub trait AmountConstraint {
    ///
    fn valid_range() -> Range<i64>;

    ///
    fn validate(value: i64) -> Result<i64, Error> {
        let range = Self::valid_range();

        if !range.contains(&value) {
            Err(Error::Contains { range, value })
        } else {
            Ok(value)
        }
    }
}
