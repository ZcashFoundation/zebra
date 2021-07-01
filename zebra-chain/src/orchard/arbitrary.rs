use group::prime::PrimeCurveAffine;
use halo2::pasta::pallas;
use proptest::{arbitrary::any, array, collection::vec, prelude::*};
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

use crate::primitives::redpallas::{
    Signature, SigningKey, SpendAuth, VerificationKey, VerificationKeyBytes,
};

use super::{keys, note, tree, Action, AuthorizedAction, Flags, NoteCommitment, ValueCommitment};

use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
};

impl Arbitrary for Action {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<note::Nullifier>(),
            any::<VerificationKeyBytes<SpendAuth>>(),
            any::<note::EncryptedNote>(),
            any::<note::WrappedNoteKey>(),
        )
            .prop_map(|(nullifier, rk, enc_ciphertext, out_ciphertext)| Self {
                cv: ValueCommitment(pallas::Affine::identity()),
                nullifier,
                rk,
                cm_x: NoteCommitment(pallas::Affine::identity()).extract_x(),
                ephemeral_key: keys::EphemeralPublicKey(pallas::Affine::identity()),
                enc_ciphertext,
                out_ciphertext,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for note::Nullifier {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        use halo2::arithmetic::FieldExt;

        (vec(any::<u8>(), 64))
            .prop_map(|bytes| {
                let bytes = bytes.try_into().expect("vec is the correct length");
                Self::try_from(pallas::Scalar::from_bytes_wide(&bytes).to_bytes())
                    .expect("a valid generated nullifier")
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for AuthorizedAction {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<Action>(), any::<Signature<SpendAuth>>())
            .prop_map(|(action, spend_auth_sig)| Self {
                action,
                spend_auth_sig,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Signature<SpendAuth> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (array::uniform32(any::<u8>()), array::uniform32(any::<u8>()))
            .prop_map(|(r_bytes, s_bytes)| Self {
                r_bytes,
                s_bytes,
                _marker: PhantomData,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for VerificationKeyBytes<SpendAuth> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (prop::array::uniform32(any::<u8>()))
            .prop_map(|bytes| {
                let mut rng = ChaChaRng::from_seed(bytes);
                let sk = SigningKey::<SpendAuth>::new(&mut rng);
                let pk = VerificationKey::from(&sk);
                pk.into()
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Flags {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<u8>()).prop_map(Self::from_bits_truncate).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for tree::Root {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        use halo2::arithmetic::FieldExt;

        (vec(any::<u8>(), 64))
            .prop_map(|bytes| {
                let bytes = bytes.try_into().expect("vec is the correct length");
                Self::try_from(pallas::Base::from_bytes_wide(&bytes).to_bytes())
                    .expect("a valid generated Orchard note commitment tree root")
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
