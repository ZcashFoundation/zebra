use proptest::{arbitrary::any, array, collection::vec, prelude::*};

use crate::notes::sapling;

impl Arbitrary for sapling::EncryptedCiphertext {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 580))
            .prop_map(|v| {
                let mut bytes = [0; 580];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for sapling::OutCiphertext {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 80))
            .prop_map(|v| {
                let mut bytes = [0; 80];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

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
