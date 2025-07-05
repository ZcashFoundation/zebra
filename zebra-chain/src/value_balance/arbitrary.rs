use crate::{amount::*, value_balance::*};
use proptest::prelude::*;

impl Arbitrary for ValueBalance<NegativeAllowed> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount<NegativeAllowed>>(),
            any::<Amount<NegativeAllowed>>(),
            any::<Amount<NegativeAllowed>>(),
            any::<Amount<NegativeAllowed>>(),
            any::<Amount<NegativeAllowed>>(),
            any::<Amount<NegativeAllowed>>(),
        )
            .prop_map(|(transparent, sprout, sapling, orchard, lockbox, deferred)| Self {
                transparent,
                sprout,
                sapling,
                orchard,
                lockbox,
                deferred,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for ValueBalance<NonNegative> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
        )
            .prop_map(|(transparent, sprout, sapling, orchard, lockbox, deferred)| Self {
                transparent,
                sprout,
                sapling,
                orchard,
                lockbox,
                deferred,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
