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
            #[cfg(zcash_unstable = "zip234")]
            any::<Amount<NegativeAllowed>>(),
        )
            .prop_map(
                #[cfg(not(zcash_unstable = "zip234"))]
                |(transparent, sprout, sapling, orchard, deferred, ironwood)| Self {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                },
                #[cfg(zcash_unstable = "zip234")]
                |(transparent, sprout, sapling, orchard, deferred, ironwood, nsm)| Self {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                    nsm,
                },
            )
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
            #[cfg(zcash_unstable = "zip234")]
            any::<Amount<NonNegative>>(),
        )
            .prop_map(
                #[cfg(not(zcash_unstable = "zip234"))]
                |(transparent, sprout, sapling, orchard, deferred, ironwood)| Self {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                },
                #[cfg(zcash_unstable = "zip234")]
                |(transparent, sprout, sapling, orchard, deferred, ironwood, nsm)| Self {
                    transparent,
                    sprout,
                    sapling,
                    orchard,
                    deferred,
                    ironwood,
                    nsm,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
