//! Block height.

use std::ops::{Add, Sub};

use crate::serialization::SerializationError;

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
pub struct Height(pub u32);

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
}

/// A difference between two [`Height`]s, possibly negative.
pub type HeightDiff = i32;

// impl TryFrom<Height> for HeightDiff {
//     type Error = TryFromIntError;

//     fn try_from(height: Height) -> Result<Self, Self::Error> {
//         HeightDiff::try_from(height.0)
//     }
// }

impl Sub<Height> for Height {
    type Output = Option<HeightDiff>;

    fn sub(self, rhs: Height) -> Self::Output {
        // We convert the heights from [`u32`] to [`i32`] because `u32::checked_sub` returns
        // [`None`] for negative results. We must check the conversion since it's possible to
        // construct heights outside the valid range for [`i32`].
        let lhs = i32::try_from(self.0).ok()?;
        let rhs = i32::try_from(rhs.0).ok()?;
        lhs.checked_sub(rhs)
    }
}

impl Sub<HeightDiff> for Height {
    type Output = Option<Self>;

    fn sub(self, rhs: HeightDiff) -> Option<Self> {
        // We need to convert the height to [`i32`] so we can subtract negative [`HeightDiff`]s. We
        // must check the conversion since it's possible to construct heights outside the valid
        // range for [`i32`].
        let lhs = i32::try_from(self.0).ok()?;
        let res = lhs.checked_sub(rhs)?;
        let res = u32::try_from(res).ok()?;
        let height = Height(res);

        // Check the bounds.
        if Height::MIN <= height && height <= Height::MAX {
            Some(height)
        } else {
            None
        }
    }
}

impl Add<HeightDiff> for Height {
    type Output = Option<Height>;

    fn add(self, rhs: HeightDiff) -> Option<Height> {
        // We need to convert the height to [`i32`] so we can subtract negative [`HeightDiff`]s. We
        // must check the conversion since it's possible to construct heights outside the valid
        // range for [`i32`].
        let lhs = i32::try_from(self.0).ok()?;
        let res = lhs.checked_add(rhs)?;
        let res = u32::try_from(res).ok()?;
        let height = Height(res);

        // Check the bounds.
        if Height::MIN <= height && height <= Height::MAX {
            Some(height)
        } else {
            None
        }
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

    assert_eq!(Some(Height(2)), Height(1) + 1);
    assert_eq!(None, Height::MAX + 1);

    // Adding negative numbers
    assert_eq!(Some(Height(1)), Height(2) + -1);
    assert_eq!(Some(Height(0)), Height(1) + -1);
    assert_eq!(None, Height(0) + -1);
    assert_eq!(Some(Height(Height::MAX_AS_U32 - 1)), Height::MAX + -1);

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract
    // `+ 0` would also cause an error here, but it triggers a spurious clippy lint
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) + 1);
    assert_eq!(None, Height(i32::MAX as u32) + 1);
    assert_eq!(None, Height(u32::MAX) + 1);

    // Adding negative numbers
    assert_eq!(None, Height(i32::MAX as u32 + 1) + -1);
    assert_eq!(None, Height(u32::MAX) + -1);

    assert_eq!(Some(Height(1)), Height(2) - 1);
    assert_eq!(Some(Height(0)), Height(1) - 1);
    assert_eq!(None, Height(0) - 1);
    assert_eq!(Some(Height(Height::MAX_AS_U32 - 1)), Height::MAX - 1);

    // Subtracting negative numbers
    assert_eq!(Some(Height(2)), Height(1) - -1);
    assert_eq!(Some(Height::MAX), Height(Height::MAX_AS_U32 - 1) - -1);
    assert_eq!(None, Height::MAX - -1);

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract
    assert_eq!(None, Height(i32::MAX as u32 + 1) - 1);
    assert_eq!(None, Height(u32::MAX) - 1);

    // Subtracting negative numbers
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) - -1);
    assert_eq!(None, Height(i32::MAX as u32) - -1);
    assert_eq!(None, Height(u32::MAX) - -1);

    assert_eq!(1, (Height(2) - Height(1)).unwrap());
    assert_eq!(0, (Height(1) - Height(1)).unwrap());
    assert_eq!(Some(-1), Height(0) - Height(1));
    assert_eq!(Some(-5), Height(2) - Height(7));
    assert_eq!(Height::MAX, (Height::MAX - 0).unwrap());
    assert_eq!(1, (Height::MAX - Height(Height::MAX_AS_U32 - 1)).unwrap());
    assert_eq!(Some(-1), Height(Height::MAX_AS_U32 - 1) - Height::MAX);
    assert_eq!(Some(-(Height::MAX_AS_U32 as i32)), Height(0) - Height::MAX);
}
