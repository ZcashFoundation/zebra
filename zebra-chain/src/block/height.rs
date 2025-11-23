//! Block height.

use std::ops::{Add, Sub};
use thiserror::Error;
use zcash_protocol::consensus::BlockHeight;

use crate::{serialization::SerializationError, BoxError};

#[cfg(feature = "json-conversion")]
pub mod json_conversion;

/// The length of the chain back to the genesis block.
///
/// Two [`Height`]s can't be added, but they can be *subtracted* to get their difference,
/// represented as an [`HeightDiff`]. This difference can then be added to or subtracted from a
/// [`Height`]. Note the similarity with `chrono::DateTime` and `chrono::Duration`.
///
/// # Invariants
///
/// Users should not construct block heights greater than `Height::MAX`.
///
/// # Consensus
///
/// There are multiple formats for serializing a height, so we don't implement
/// `ZcashSerialize` or `ZcashDeserialize` for `Height`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Default))]
pub struct Height(pub u32);

#[derive(Error, Debug)]
pub enum HeightError {
    #[error("The resulting height would overflow Height::MAX.")]
    Overflow,
    #[error("The resulting height would underflow Height::MIN.")]
    Underflow,
}

impl std::str::FromStr for Height {
    type Err = SerializationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(h) if (Height(h) <= Height::MAX) => Ok(Height(h)),
            Ok(_) => Err(SerializationError::Parse("Height exceeds maximum height")),
            Err(_) => Err(SerializationError::Parse("Height(u32) integer parse error")),
        }
    }
}

impl Height {
    /// The minimum [`Height`].
    ///
    /// Due to the underlying type, it is impossible to construct block heights
    /// less than [`Height::MIN`].
    ///
    /// Style note: Sometimes, [`Height::MIN`] is less readable than
    /// `Height(0)`. Use whichever makes sense in context.
    pub const MIN: Height = Height(0);

    /// The maximum [`Height`].
    ///
    /// Users should not construct block heights greater than [`Height::MAX`].
    ///
    /// The spec says *"Implementations MUST support block heights up to and
    /// including 2^31 − 1"*.
    ///
    /// Note that `u32::MAX / 2 == 2^31 - 1 == i32::MAX`.
    pub const MAX: Height = Height(u32::MAX / 2);

    /// The maximum [`Height`] as a [`u32`], for range patterns.
    ///
    /// `Height::MAX.0` can't be used in match range patterns, use this
    /// alias instead.
    pub const MAX_AS_U32: u32 = Self::MAX.0;

    /// The maximum expiration [`Height`] that is allowed in all transactions
    /// previous to Nu5 and in non-coinbase transactions from Nu5 activation
    /// height and above.
    pub const MAX_EXPIRY_HEIGHT: Height = Height(499_999_999);

    /// Returns the next [`Height`].
    ///
    /// # Panics
    ///
    /// - If the current height is at its maximum.
    pub fn next(self) -> Result<Self, HeightError> {
        (self + 1).ok_or(HeightError::Overflow)
    }

    /// Returns the previous [`Height`].
    ///
    /// # Panics
    ///
    /// - If the current height is at its minimum.
    pub fn previous(self) -> Result<Self, HeightError> {
        (self - 1).ok_or(HeightError::Underflow)
    }

    /// Returns `true` if the [`Height`] is at its minimum.
    pub fn is_min(self) -> bool {
        self == Self::MIN
    }

    /// Returns the value as a `usize`.
    pub fn as_usize(self) -> usize {
        self.0.try_into().expect("fits in usize")
    }
}

impl From<Height> for BlockHeight {
    fn from(height: Height) -> Self {
        BlockHeight::from_u32(height.0)
    }
}

impl TryFrom<BlockHeight> for Height {
    type Error = SerializationError;

    /// Checks that the `height` is within the valid [`Height`] range.
    fn try_from(height: BlockHeight) -> Result<Self, Self::Error> {
        Self::try_from(u32::from(height))
    }
}

/// A difference between two [`Height`]s, possibly negative.
///
/// This can represent the difference between any height values,
/// even if they are outside the valid height range (for example, in buggy RPC code).
pub type HeightDiff = i64;

// We don't implement TryFrom<u64>, because it causes type inference issues for integer constants.
// Instead, use 1u64.try_into_height().

impl TryFrom<u32> for Height {
    type Error = SerializationError;

    /// Checks that the `height` is within the valid [`Height`] range.
    fn try_from(height: u32) -> Result<Self, Self::Error> {
        (height <= Height::MAX.0)
            .then_some(Height(height))
            .ok_or(SerializationError::InvalidHeight)
    }
}

/// Convenience trait for converting a type into a valid Zcash [`Height`].
pub trait TryIntoHeight {
    /// The error type returned by [`Height`] conversion failures.
    type Error;

    /// Convert `self` to a `Height`, if possible.
    fn try_into_height(&self) -> Result<Height, Self::Error>;
}

impl TryIntoHeight for u64 {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        u32::try_from(*self)?.try_into().map_err(Into::into)
    }
}

impl TryIntoHeight for usize {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        u32::try_from(*self)?.try_into().map_err(Into::into)
    }
}

impl TryIntoHeight for str {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        self.parse().map_err(Into::into)
    }
}

impl TryIntoHeight for String {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        self.as_str().try_into_height()
    }
}

impl TryIntoHeight for i32 {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        u32::try_from(*self)?.try_into().map_err(Into::into)
    }
}

// We don't implement Add<u32> or Sub<u32>, because they cause type inference issues for integer constants.

impl Sub<Height> for Height {
    type Output = HeightDiff;

    /// Subtract two heights, returning the result, which can be negative.
    /// Since [`HeightDiff`] is `i64` and [`Height`] is `u32`, the result is always correct.
    fn sub(self, rhs: Height) -> Self::Output {
        // All these conversions are exact, and the subtraction can't overflow or underflow.
        let lhs = HeightDiff::from(self.0);
        let rhs = HeightDiff::from(rhs.0);

        lhs - rhs
    }
}

impl Sub<HeightDiff> for Height {
    type Output = Option<Self>;

    /// Subtract a height difference from a height, returning `None` if the resulting height is
    /// outside the valid `Height` range (this also checks the result is non-negative).
    fn sub(self, rhs: HeightDiff) -> Option<Self> {
        // We need to convert the height to [`i64`] so we can subtract negative [`HeightDiff`]s.
        let lhs = HeightDiff::from(self.0);
        let res = lhs - rhs;

        // Check the bounds.
        let res = u32::try_from(res).ok()?;
        Height::try_from(res).ok()
    }
}

impl Add<HeightDiff> for Height {
    type Output = Option<Height>;

    /// Add a height difference to a height, returning `None` if the resulting height is outside
    /// the valid `Height` range (this also checks the result is non-negative).
    fn add(self, rhs: HeightDiff) -> Option<Height> {
        // We need to convert the height to [`i64`] so we can add negative [`HeightDiff`]s.
        let lhs = i64::from(self.0);
        let res = lhs + rhs;

        // Check the bounds.
        let res = u32::try_from(res).ok()?;
        Height::try_from(res).ok()
    }
}

#[test]
fn operator_tests() {
    let _init_guard = zebra_test::init();

    // Elementary checks.
    assert_eq!(Some(Height(2)), Height(1) + 1);
    assert_eq!(None, Height::MAX + 1);

    let height = Height(u32::pow(2, 31) - 2);
    assert!(height < Height::MAX);

    let max_height = (height + 1).expect("this addition should produce the max height");
    assert!(height < max_height);
    assert!(max_height <= Height::MAX);
    assert_eq!(Height::MAX, max_height);
    assert_eq!(None, max_height + 1);

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) + 0);
    assert_eq!(None, Height(i32::MAX as u32) + 1);
    assert_eq!(None, Height(u32::MAX) + 0);

    // Adding negative numbers
    assert_eq!(Some(Height(1)), Height(2) + -1);
    assert_eq!(Some(Height(0)), Height(1) + -1);
    assert_eq!(None, Height(0) + -1);
    assert_eq!(Some(Height(Height::MAX_AS_U32 - 1)), Height::MAX + -1);

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract,
    // and the result is invalid
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) + 1);
    assert_eq!(None, Height(i32::MAX as u32) + 1);
    assert_eq!(None, Height(u32::MAX) + 1);

    // Adding negative numbers
    assert_eq!(Some(Height::MAX), Height(i32::MAX as u32 + 1) + -1);
    assert_eq!(None, Height(u32::MAX) + -1);

    assert_eq!(Some(Height(1)), Height(2) - 1);
    assert_eq!(Some(Height(0)), Height(1) - 1);
    assert_eq!(None, Height(0) - 1);
    assert_eq!(Some(Height(Height::MAX_AS_U32 - 1)), Height::MAX - 1);

    // Subtracting negative numbers
    assert_eq!(Some(Height(2)), Height(1) - -1);
    assert_eq!(Some(Height::MAX), Height(Height::MAX_AS_U32 - 1) - -1);
    assert_eq!(None, Height::MAX - -1);

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract,
    // and the result is invalid
    assert_eq!(Some(Height::MAX), Height(i32::MAX as u32 + 1) - 1);
    assert_eq!(None, Height(u32::MAX) - 1);

    // Subtracting negative numbers
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) - -1);
    assert_eq!(None, Height(i32::MAX as u32) - -1);
    assert_eq!(None, Height(u32::MAX) - -1);

    assert_eq!(1, (Height(2) - Height(1)));
    assert_eq!(0, (Height(1) - Height(1)));
    assert_eq!(-1, Height(0) - Height(1));
    assert_eq!(-5, Height(2) - Height(7));
    assert_eq!(Height::MAX.0 as HeightDiff, (Height::MAX - Height(0)));
    assert_eq!(1, (Height::MAX - Height(Height::MAX_AS_U32 - 1)));
    assert_eq!(-1, Height(Height::MAX_AS_U32 - 1) - Height::MAX);
    assert_eq!(-(Height::MAX_AS_U32 as HeightDiff), Height(0) - Height::MAX);
}
