use std::convert::TryFrom;

use proptest::{arbitrary::any, array, prelude::*};

use super::*;

impl Arbitrary for EphemeralPublicKey {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        array::uniform32(any::<u8>())
            .prop_filter_map("Valid jubjub::AffinePoint", |b| Self::try_from(b).ok())
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
