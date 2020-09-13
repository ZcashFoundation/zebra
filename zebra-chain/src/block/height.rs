use crate::serialization::SerializationError;

use std::ops::{Add, Sub};

/// The height of a block is the length of the chain back to the genesis block.
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

// Block heights live in a torsor, they can only be contructed but not computed directly.
// Addition and substraction is done in the underlying types(u32) and not in the heights themselves.
// This u32 numbers are a group and they can be added and substracted normally to get a new integer.
// Having the result of the computation the `Height(X)` constructor can be used to build a new `Height`.

// N = Height1.U - Height2.U
// Given two `Height`s use the underlying value of them to compute the difference,
// return this number as it is.
impl Sub<Height> for Height {
    type Output = Option<u32>;

    fn sub(self, rhs: Height) -> Option<u32> {
        self.0.checked_sub(rhs.0)
    }
}

// H = Height(Height.U + N)
// Given a `Height` and a number, sum the underlying value to the number,
// with the result construct a new `Height` and return it.
impl Add<u32> for Height {
    type Output = Option<Height>;

    fn add(self, rhs: u32) -> Option<Height> {
        let result = self.0.checked_add(rhs);
        match result {
            Some(h) if (Height(h) <= Height::MAX) => Some(Height(h)),
            Some(_) => None,
            None => None,
        }
    }
}

// H = Height(Height.U - N)
// Given a `Height` and a number, substract the number to the underlying value,
// with the result construct a new `Height` and return it.
impl Sub<u32> for Height {
    type Output = Option<Height>;

    fn sub(self, rhs: u32) -> Option<Height> {
        let result = self.0.checked_sub(rhs);
        match result {
            Some(h) if (Height(h) >= Height::MIN) => Some(Height(h)),
            Some(_) => None,
            None => None,
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

    assert_eq!(Some(1), Height(2) - Height(1));
    assert_eq!(Some(0), Height(1) - Height(1));
    assert_eq!(None, Height(0) - Height(1));
}
