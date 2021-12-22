use std::convert::TryInto;

use group::Group;
use jubjub::{AffinePoint, ExtendedPoint};
use proptest::{arbitrary::any, collection::vec, prelude::*};
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

use crate::primitives::Groth16Proof;

use super::{
    keys::{self, ValidatingKey},
    note, tree, FieldNotPresent, NoteCommitment, Output, OutputInTransactionV4, PerSpendAnchor,
    SharedAnchor, Spend,
};

impl Arbitrary for Spend<PerSpendAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<tree::Root>(),
            any::<note::Nullifier>(),
            spendauth_verification_key_bytes(),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(per_spend_anchor, nullifier, rk, proof, sig_bytes)| Self {
                per_spend_anchor,
                cv: ExtendedPoint::generator().try_into().unwrap(),
                nullifier,
                rk,
                zkproof: proof,
                spend_auth_sig: redjubjub::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Spend<SharedAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<note::Nullifier>(),
            spendauth_verification_key_bytes(),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(nullifier, rk, proof, sig_bytes)| Self {
                per_spend_anchor: FieldNotPresent,
                cv: ExtendedPoint::generator().try_into().unwrap(),
                nullifier,
                rk,
                zkproof: proof,
                spend_auth_sig: redjubjub::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Output {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<note::EncryptedNote>(),
            any::<note::WrappedNoteKey>(),
            any::<Groth16Proof>(),
        )
            .prop_map(|(enc_ciphertext, out_ciphertext, zkproof)| Self {
                cv: ExtendedPoint::generator().try_into().unwrap(),
                cm_u: NoteCommitment(AffinePoint::identity()).extract_u(),
                ephemeral_key: keys::EphemeralPublicKey(
                    ExtendedPoint::generator().try_into().unwrap(),
                ),
                enc_ciphertext,
                out_ciphertext,
                zkproof,
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for OutputInTransactionV4 {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        any::<Output>().prop_map(OutputInTransactionV4).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// Creates Strategy for generation VerificationKeyBytes, since the `redjubjub`
/// crate does not provide an Arbitrary implementation for it.
fn spendauth_verification_key_bytes() -> impl Strategy<Value = ValidatingKey> {
    prop::array::uniform32(any::<u8>()).prop_map(|bytes| {
        let mut rng = ChaChaRng::from_seed(bytes);
        let sk = redjubjub::SigningKey::<redjubjub::SpendAuth>::new(&mut rng);
        redjubjub::VerificationKey::<redjubjub::SpendAuth>::from(&sk)
            .try_into()
            .unwrap()
    })
}

impl Arbitrary for tree::Root {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 64))
            .prop_map(|bytes| {
                let bytes = bytes.try_into().expect("vec is the correct length");
                jubjub::Fq::from_bytes_wide(&bytes).to_bytes().into()
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
