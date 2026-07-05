//! Randomised data generation for Ironwood types.

use proptest::prelude::*;

use crate::{
    ironwood::{Nullifier, ShieldedData},
    orchard,
};

impl Arbitrary for Nullifier {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<orchard::Nullifier>().prop_map(Nullifier).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for ShieldedData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<orchard::ShieldedData>(), any::<u8>())
            .prop_map(|(mut shielded_data, flag_bits)| {
                // The base Orchard strategy only generates the pre-NU6.3 flag bits, because
                // Orchard-pool bundles (v5 and v6) reserve `enableCrossAddress`. The Ironwood
                // bundle is the only place that flag is valid, so re-generate the flags over
                // all three defined bits to exercise the `FlagsV6` codec path.
                shielded_data.flags = orchard::Flags::from_bits_truncate(flag_bits);
                Self::new(orchard::ShieldedDataV6::new(shielded_data))
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
