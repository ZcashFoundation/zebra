//! Block height.

use crate::serialization::{SerializationError, ZcashDeserialize};

use byteorder::{LittleEndian, ReadBytesExt};

use std::{
    convert::TryFrom,
    io,
    ops::{Add, Sub},
};

/// The length of the chain back to the genesis block.
///
/// Block heights can't be added, but they can be *subtracted*,
/// to get a difference of block heights, represented as an `i32`,
/// and height differences can be added to block heights to get new heights.
///
/// # Invariants
///
/// Users should not construct block heights greater than `Height::MAX`.
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

impl Add<Height> for Height {
    type Output = Option<Height>;

    fn add(self, rhs: Height) -> Option<Height> {
        // We know that both values are positive integers. Therefore, the result is
        // positive, and we can skip the conversions. The checked_add is required,
        // because the result may overflow.
        let height = self.0.checked_add(rhs.0)?;
        let height = Height(height);

        if height <= Height::MAX && height >= Height::MIN {
            Some(height)
        } else {
            None
        }
    }
}

impl Sub<Height> for Height {
    type Output = i32;

    /// Panics if the inputs or result are outside the valid i32 range.
    fn sub(self, rhs: Height) -> i32 {
        // We construct heights from integers without any checks,
        // so the inputs or result could be out of range.
        let lhs = i32::try_from(self.0)
            .expect("out of range input `self`: inputs should be valid Heights");
        let rhs =
            i32::try_from(rhs.0).expect("out of range input `rhs`: inputs should be valid Heights");
        lhs.checked_sub(rhs)
            .expect("out of range result: valid input heights should yield a valid result")
    }
}

// We don't implement Add<u32> or Sub<u32>, because they cause type inference issues for integer constants.

impl Add<i32> for Height {
    type Output = Option<Height>;

    fn add(self, rhs: i32) -> Option<Height> {
        // Because we construct heights from integers without any checks,
        // the input values could be outside the valid range for i32.
        let lhs = i32::try_from(self.0).ok()?;
        let result = lhs.checked_add(rhs)?;
        let result = u32::try_from(result).ok()?;
        match result {
            h if (Height(h) <= Height::MAX && Height(h) >= Height::MIN) => Some(Height(h)),
            _ => None,
        }
    }
}

impl Sub<i32> for Height {
    type Output = Option<Height>;

    fn sub(self, rhs: i32) -> Option<Height> {
        // These checks are required, see above for details.
        let lhs = i32::try_from(self.0).ok()?;
        let result = lhs.checked_sub(rhs)?;
        let result = u32::try_from(result).ok()?;
        match result {
            h if (Height(h) <= Height::MAX && Height(h) >= Height::MIN) => Some(Height(h)),
            _ => None,
        }
    }
}

impl ZcashDeserialize for Height {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let height = reader.read_u32::<LittleEndian>()?;

        if height > Self::MAX.0 {
            return Err(SerializationError::Parse("Height exceeds maximum height"));
        }

        Ok(Self(height))
    }
}

#[test]
fn operator_tests() {
    let _init_guard = zebra_test::init();

    // Elementary checks.
    assert_eq!(Some(Height(2)), Height(1) + Height(1));
    assert_eq!(None, Height::MAX + Height(1));

    let height = Height(u32::pow(2, 31) - 2);
    assert!(height < Height::MAX);

    let max_height = (height + Height(1)).expect("this addition should produce the max height");
    assert!(height < max_height);
    assert!(max_height <= Height::MAX);
    assert_eq!(Height::MAX, max_height);
    assert_eq!(None, max_height + Height(1));

    // Bad heights aren't caught at compile-time or runtime, until we add or subtract
    assert_eq!(None, Height(Height::MAX_AS_U32 + 1) + Height(0));
    assert_eq!(None, Height(i32::MAX as u32) + Height(1));
    assert_eq!(None, Height(u32::MAX) + Height(0));

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

    // Sub<Height> panics on out of range errors
    assert_eq!(1, Height(2) - Height(1));
    assert_eq!(0, Height(1) - Height(1));
    assert_eq!(-1, Height(0) - Height(1));
    assert_eq!(-5, Height(2) - Height(7));
    assert_eq!(Height::MAX_AS_U32 as i32, Height::MAX - Height(0));
    assert_eq!(1, Height::MAX - Height(Height::MAX_AS_U32 - 1));
    assert_eq!(-1, Height(Height::MAX_AS_U32 - 1) - Height::MAX);
    assert_eq!(-(Height::MAX_AS_U32 as i32), Height(0) - Height::MAX);
}
