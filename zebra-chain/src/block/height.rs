use crate::serialization::SerializationError;

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
