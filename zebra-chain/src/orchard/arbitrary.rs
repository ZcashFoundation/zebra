use group::prime::PrimeCurveAffine;
use halo2::pasta::pallas;
use proptest::{arbitrary::any, array, prelude::*};

use super::{keys, note, Action, NoteCommitment, ValueCommitment};

impl Arbitrary for Action {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<note::Nullifier>(),
            array::uniform32(any::<u8>()),
            any::<note::EncryptedNote>(),
            any::<note::WrappedNoteKey>(),
        )
            .prop_map(
                |(nullifier, rpk_bytes, enc_ciphertext, out_ciphertext)| Self {
                    cv: ValueCommitment(pallas::Affine::identity()),
                    nullifier,
                    rk: crate::primitives::redpallas::VerificationKeyBytes::from(rpk_bytes),
                    cm_x: NoteCommitment(pallas::Affine::identity()).extract_x(),
                    ephemeral_key: keys::EphemeralPublicKey(pallas::Affine::identity()),
                    enc_ciphertext,
                    out_ciphertext,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for note::Nullifier {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        array::uniform32(any::<u8>()).prop_map(Self::from).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
