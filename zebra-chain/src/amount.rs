//! Strongly-typed zatoshi amounts that prevent under/overflows.
//!
//! The [`Amount`] type is parameterized by a [`Constraint`] implementation that
//! declares the range of allowed values. In contrast to regular arithmetic
//! operations, which return values, arithmetic on [`Amount`]s returns
//! [`Result`](std::result::Result)s.

use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::RangeInclusive,
};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[cfg(test)]
mod tests;

/// The result of an amount operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A runtime validated type for representing amounts of zatoshis
//
// TODO:
// - remove the default NegativeAllowed bound, to make consensus rule reviews easier
// - put a Constraint bound on the type generic, not just some implementations
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
#[serde(try_from = "i64")]
#[serde(into = "i64")]
#[serde(bound = "C: Constraint + Clone")]
pub struct Amount<C = NegativeAllowed>(
    /// The inner amount value.
    i64,
    /// Used for [`Constraint`] type inference.
    ///
    /// # Correctness
    ///
    /// This internal Zebra marker type is not consensus-critical.
    /// And it should be ignored during testing. (And other internal uses.)
    #[serde(skip)]
    PhantomData<C>,
);

impl<C> fmt::Display for Amount<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let zats = self.zatoshis();

        f.pad_integral(zats > 0, "", &zats.to_string())
    }
}

impl<C> fmt::Debug for Amount<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(&format!("Amount<{}>", std::any::type_name::<C>()))
            .field(&self.0)
            .finish()
    }
}

impl Amount<NonNegative> {
    /// Create a new non-negative [`Amount`] from a provided value in ZEC.
    pub const fn new_from_zec(zec_value: i64) -> Self {
        Self::new(zec_value.checked_mul(COIN).expect("should fit in i64"))
    }

    /// Create a new non-negative [`Amount`] from a provided value in zatoshis.
    pub const fn new(zatoshis: i64) -> Self {
        assert!(zatoshis <= MAX_MONEY && zatoshis >= 0);
        Self(zatoshis, PhantomData)
    }

    /// Divide an [`Amount`] by a value that the amount fits into evenly such that there is no remainder.
    pub const fn div_exact(self, rhs: i64) -> Self {
        let result = self.0.checked_div(rhs).expect("divisor must be non-zero");
        if self.0 % rhs != 0 {
            panic!("divisor must divide amount evenly, no remainder");
        }

        Self(result, PhantomData)
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

    /// Returns the number of zatoshis in this amount.
    pub fn zatoshis(&self) -> i64 {
        self.0
    }

    /// Checked subtraction. Computes self - rhs, returning None if overflow occurred.
    pub fn checked_sub<C2: Constraint>(self, rhs: Amount<C2>) -> Option<Amount> {
        self.0.checked_sub(rhs.0).and_then(|v| v.try_into().ok())
    }

    /// To little endian byte array
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut buf: [u8; 8] = [0; 8];
        LittleEndian::write_i64(&mut buf, self.0);
        buf
    }

    /// From little endian byte array
    pub fn from_bytes(bytes: [u8; 8]) -> Result<Amount<C>>
    where
        C: Constraint,
    {
        let amount = i64::from_le_bytes(bytes);
        amount.try_into()
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
        let value = self
            .0
            .checked_add(rhs.0)
            .expect("adding two constrained Amounts is always within an i64");
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
        let value = self
            .0
            .checked_sub(rhs.0)
            .expect("subtracting two constrained Amounts is always within an i64");
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
        amount.0.try_into().expect("non-negative i64 fits in u64")
    }
}

impl<C> From<Amount<C>> for jubjub::Fr {
    fn from(a: Amount<C>) -> jubjub::Fr {
        // TODO: this isn't constant time -- does that matter?
        if a.0 < 0 {
            let abs_amount = i128::from(a.0)
                .checked_abs()
                .expect("absolute i64 fits in i128");
            let abs_amount = u64::try_from(abs_amount).expect("absolute i64 fits in u64");

            jubjub::Fr::from(abs_amount).neg()
        } else {
            jubjub::Fr::from(u64::try_from(a.0).expect("non-negative i64 fits in u64"))
        }
    }
}

impl<C> From<Amount<C>> for halo2::pasta::pallas::Scalar {
    fn from(a: Amount<C>) -> halo2::pasta::pallas::Scalar {
        // TODO: this isn't constant time -- does that matter?
        if a.0 < 0 {
            let abs_amount = i128::from(a.0)
                .checked_abs()
                .expect("absolute i64 fits in i128");
            let abs_amount = u64::try_from(abs_amount).expect("absolute i64 fits in u64");

            halo2::pasta::pallas::Scalar::from(abs_amount).neg()
        } else {
            halo2::pasta::pallas::Scalar::from(
                u64::try_from(a.0).expect("non-negative i64 fits in u64"),
            )
        }
    }
}

impl<C> TryFrom<i32> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        C::validate(value.into()).map(|v| Self(v, PhantomData))
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

impl<C> TryFrom<u64> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let value = value.try_into().map_err(|source| Error::Convert {
            value: value.into(),
            source,
        })?;

        C::validate(value).map(|v| Self(v, PhantomData))
    }
}

/// Conversion from `i128` to `Amount`.
///
/// Used to handle the result of multiplying negative `Amount`s by `u64`.
impl<C> TryFrom<i128> for Amount<C>
where
    C: Constraint,
{
    type Error = Error;

    fn try_from(value: i128) -> Result<Self, Self::Error> {
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

impl<C> Eq for Amount<C> {}

impl<C1, C2> PartialOrd<Amount<C2>> for Amount<C1> {
    fn partial_cmp(&self, other: &Amount<C2>) -> Option<Ordering> {
        Some(self.0.cmp(&other.0))
    }
}

impl<C> Ord for Amount<C> {
    fn cmp(&self, other: &Amount<C>) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl<C> std::ops::Mul<u64> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn mul(self, rhs: u64) -> Self::Output {
        // use i128 for multiplication, so we can handle negative Amounts
        let value = i128::from(self.0)
            .checked_mul(i128::from(rhs))
            .expect("multiplying i64 by u64 can't overflow i128");

        value.try_into().map_err(|_| Error::MultiplicationOverflow {
            amount: self.0,
            multiplier: rhs,
            overflowing_result: value,
        })
    }
}

impl<C> std::ops::Mul<Amount<C>> for u64
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn mul(self, rhs: Amount<C>) -> Self::Output {
        rhs.mul(self)
    }
}

impl<C> std::ops::Div<u64> for Amount<C>
where
    C: Constraint,
{
    type Output = Result<Amount<C>>;

    fn div(self, rhs: u64) -> Self::Output {
        let quotient = i128::from(self.0)
            .checked_div(i128::from(rhs))
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
    fn sum<I: Iterator<Item = Amount<C>>>(mut iter: I) -> Self {
        let sum = iter.try_fold(Amount::zero(), |acc, amount| acc + amount);

        match sum {
            Ok(sum) => Ok(sum),
            Err(Error::Constraint { value, .. }) => Err(Error::SumOverflow {
                partial_sum: value,
                remaining_items: iter.count(),
            }),
            Err(unexpected_error) => unreachable!("unexpected Add error: {:?}", unexpected_error),
        }
    }
}

impl<'amt, C> std::iter::Sum<&'amt Amount<C>> for Result<Amount<C>>
where
    C: Constraint + Copy + 'amt,
{
    fn sum<I: Iterator<Item = &'amt Amount<C>>>(iter: I) -> Self {
        iter.copied().sum()
    }
}

// TODO: add infallible impls for NonNegative <-> NegativeOrZero,
//       when Rust uses trait output types to disambiguate overlapping impls.
impl<C> std::ops::Neg for Amount<C>
where
    C: Constraint,
{
    type Output = Amount<NegativeAllowed>;
    fn neg(self) -> Self::Output {
        Amount::<NegativeAllowed>::try_from(-self.0)
            .expect("a negation of any Amount into NegativeAllowed is always valid")
    }
}

#[allow(missing_docs)]
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
/// Errors that can be returned when validating [`Amount`]s.
pub enum Error {
    /// input {value} is outside of valid range for zatoshi Amount, valid_range={range:?}
    Constraint {
        value: i64,
        range: RangeInclusive<i64>,
    },

    /// {value} could not be converted to an i64 Amount
    Convert {
        value: i128,
        source: std::num::TryFromIntError,
    },

    /// i64 overflow when multiplying i64 amount {amount} by u64 {multiplier}, overflowing result {overflowing_result}
    MultiplicationOverflow {
        amount: i64,
        multiplier: u64,
        overflowing_result: i128,
    },

    /// cannot divide amount {amount} by zero
    DivideByZero { amount: i64 },

    /// i64 overflow when summing i64 amounts, partial_sum: {partial_sum}, remaining items: {remaining_items}
    SumOverflow {
        partial_sum: i64,
        remaining_items: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Error::Constraint { value, range } => format!(
                "input {value} is outside of valid range for zatoshi Amount, valid_range={range:?}"
            ),
            Error::Convert { value, .. } => {
                format!("{value} could not be converted to an i64 Amount")
            }
            Error::MultiplicationOverflow {
                amount,
                multiplier,
                overflowing_result,
            } => format!(
                "overflow when calculating {amount}i64 * {multiplier}u64 = {overflowing_result}i128"
            ),
            Error::DivideByZero { amount } => format!("cannot divide amount {amount} by zero"),
            Error::SumOverflow {
                partial_sum,
                remaining_items,
            } => format!(
                "overflow when summing i64 amounts; \
                          partial sum: {partial_sum}, number of remaining items: {remaining_items}"
            ),
        })
    }
}

impl Error {
    /// Returns the invalid value for this error.
    ///
    /// This value may be an initial input value, partially calculated value,
    /// or an overflowing or underflowing value.
    pub fn invalid_value(&self) -> i128 {
        use Error::*;

        match self.clone() {
            Constraint { value, .. } => value.into(),
            Convert { value, .. } => value,
            MultiplicationOverflow {
                overflowing_result, ..
            } => overflowing_result,
            DivideByZero { amount } => amount.into(),
            SumOverflow { partial_sum, .. } => partial_sum.into(),
        }
    }
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct NonNegative;

impl Constraint for NonNegative {
    fn valid_range() -> RangeInclusive<i64> {
        0..=MAX_MONEY
    }
}

/// Marker type for `Amount` that requires negative or zero values.
///
/// Used for coinbase transactions in `getblocktemplate` RPCs.
///
/// ```
/// # use zebra_chain::amount::{Constraint, MAX_MONEY, NegativeOrZero};
/// assert_eq!(
///     NegativeOrZero::valid_range(),
///     -MAX_MONEY..=0,
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct NegativeOrZero;

impl Constraint for NegativeOrZero {
    fn valid_range() -> RangeInclusive<i64> {
        -MAX_MONEY..=0
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
            Err(Error::Constraint { value, range })
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
    #[allow(clippy::unwrap_in_result)]
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

/// Represents a change to the deferred pool balance from a coinbase transaction.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DeferredPoolBalanceChange(Amount);

impl DeferredPoolBalanceChange {
    /// Creates a new [`DeferredPoolBalanceChange`]
    pub fn new(amount: Amount) -> Self {
        Self(amount)
    }

    /// Creates a new [`DeferredPoolBalanceChange`] with a zero value.
    pub fn zero() -> Self {
        Self(Amount::zero())
    }

    /// Consumes `self` and returns the inner [`Amount`] value.
    pub fn value(self) -> Amount {
        self.0
    }
}
