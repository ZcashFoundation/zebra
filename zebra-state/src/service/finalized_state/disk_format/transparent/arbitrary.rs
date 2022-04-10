//! Randomised data generation for disk format property tests.

use proptest::prelude::*;

use zebra_chain::{serialization::TrustedPreallocate, transparent};

use super::OutputIndex;

impl Arbitrary for OutputIndex {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (0..=transparent::Output::max_allocation())
            .prop_map(OutputIndex::from_u64)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
