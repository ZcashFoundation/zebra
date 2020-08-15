use crate::serialization::SerializationError;

/// A u32 which represents a block height value.
///
/// # Invariants
///
/// Users should not construct block heights greater than `BlockHeight::MAX`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockHeight(pub u32);

impl std::str::FromStr for BlockHeight {
    type Err = SerializationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(h) if (BlockHeight(h) <= BlockHeight::MAX) => Ok(BlockHeight(h)),
            Ok(_) => Err(SerializationError::Parse(
                "BlockHeight exceeds maximum height",
            )),
            Err(_) => Err(SerializationError::Parse(
                "BlockHeight(u32) integer parse error",
            )),
        }
    }
}

impl BlockHeight {
    /// The minimum BlockHeight.
    ///
    /// Due to the underlying type, it is impossible to construct block heights
    /// less than `BlockHeight::MIN`.
    ///
    /// Style note: Sometimes, `BlockHeight::MIN` is less readable than
    /// `BlockHeight(0)`. Use whichever makes sense in context.
    pub const MIN: BlockHeight = BlockHeight(0);

    /// The maximum BlockHeight.
    ///
    /// Users should not construct block heights greater than `BlockHeight::MAX`.
    pub const MAX: BlockHeight = BlockHeight(499_999_999);

    /// The maximum BlockHeight as a u32, for range patterns.
    ///
    /// `BlockHeight::MAX.0` can't be used in match range patterns, use this
    /// alias instead.
    pub const MAX_AS_U32: u32 = Self::MAX.0;
}

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
impl Arbitrary for BlockHeight {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (BlockHeight::MIN.0..=BlockHeight::MAX.0)
            .prop_map(BlockHeight)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
