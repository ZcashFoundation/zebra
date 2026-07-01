//! Randomised data generation for Ironwood types.

use proptest::prelude::*;

use crate::{ironwood::Nullifier, orchard};

impl Arbitrary for Nullifier {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<orchard::Nullifier>().prop_map(Nullifier).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
