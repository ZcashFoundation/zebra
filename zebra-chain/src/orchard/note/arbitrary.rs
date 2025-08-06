use proptest::{collection::vec, prelude::*};

use super::*;

impl<const SIZE: usize> Arbitrary for EncryptedNote<SIZE> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), SIZE))
            .prop_map(|v| {
                let mut bytes = [0; SIZE];
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
