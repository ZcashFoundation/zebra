use crate::notes::sapling;

use proptest::{array, prelude::*};

impl Arbitrary for sapling::NoteCommitment {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        array::uniform32(any::<u8>())
            .prop_filter("Valid jubjub::AffinePoint", |b| {
                jubjub::AffinePoint::from_bytes(*b).is_some().unwrap_u8() == 1
            })
            .prop_map(Self::from)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for sapling::ValueCommitment {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        array::uniform32(any::<u8>())
            .prop_filter("Valid jubjub::AffinePoint", |b| {
                jubjub::AffinePoint::from_bytes(*b).is_some().unwrap_u8() == 1
            })
            .prop_map(Self::from)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
