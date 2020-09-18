use crate::serialization::SerializationError;

use std::ops::{Add, Sub};

/// The height of a block is the length of the chain back to the genesis block.
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
    /// The minimum Height.
    ///
    /// Due to the underlying type, it is impossible to construct block heights
    /// less than `Height::MIN`.
    ///
    /// Style note: Sometimes, `Height::MIN` is less readable than
    /// `Height(0)`. Use whichever makes sense in context.
    pub const MIN: Height = Height(0);

    /// The maximum Height.
    ///
    /// Users should not construct block heights greater than `Height::MAX`.
    pub const MAX: Height = Height(499_999_999);

    /// The maximum Height as a u32, for range patterns.
    ///
    /// `Height::MAX.0` can't be used in match range patterns, use this
    /// alias instead.
    pub const MAX_AS_U32: u32 = Self::MAX.0;
}

impl Sub<Height> for Height {
    type Output = i32;

    fn sub(self, rhs: Height) -> i32 {
        (self.0 as i32) - (rhs.0 as i32)
    }
}

impl Add<i32> for Height {
    type Output = Option<Height>;

    fn add(self, rhs: i32) -> Option<Height> {
        let result = ((self.0 as i32) + rhs) as u32;
        match result {
            h if (Height(h) <= Height::MAX && Height(h) >= Height::MIN) => Some(Height(h)),
            _ => None,
        }
    }
}

impl Sub<i32> for Height {
    type Output = Option<Height>;

    fn sub(self, rhs: i32) -> Option<Height> {
        let result = ((self.0 as i32) - rhs) as u32;
        match result {
            h if (Height(h) <= Height::MAX && Height(h) >= Height::MIN) => Some(Height(h)),
            _ => None,
        }
    }
}

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
impl Arbitrary for Height {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (Height::MIN.0..=Height::MAX.0).prop_map(Height).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[test]
fn operator_tests() {
    assert_eq!(Some(Height(2)), Height(1) + 1);
    assert_eq!(None, Height::MAX + 1);

    assert_eq!(Some(Height(1)), Height(2) - 1);
    assert_eq!(Some(Height(0)), Height(1) - 1);
    assert_eq!(None, Height(0) - 1);

    assert_eq!(1, Height(2) - Height(1));
    assert_eq!(0, Height(1) - Height(1));
    assert_eq!(-1, Height(0) - Height(1));
}
