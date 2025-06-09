use proptest::{collection::vec, prelude::*};

use super::*;

impl<const N: usize> Arbitrary for EncryptedNote<N> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), N))
            .prop_map(|v| {
                let mut bytes = [0; N];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for WrappedNoteKey {
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
