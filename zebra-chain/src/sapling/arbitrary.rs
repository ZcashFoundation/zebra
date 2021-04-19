use jubjub::AffinePoint;
use proptest::{arbitrary::any, array, collection::vec, prelude::*};

use crate::primitives::Groth16Proof;

use super::{
    keys, note, tree, FieldNotPresent, NoteCommitment, Output, OutputInTransactionV4,
    PerSpendAnchor, SharedAnchor, Spend, ValueCommitment,
};

impl Arbitrary for Spend<PerSpendAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<tree::Root>(),
            any::<note::Nullifier>(),
            array::uniform32(any::<u8>()),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(
                |(per_spend_anchor, nullifier, rpk_bytes, proof, sig_bytes)| Self {
                    per_spend_anchor,
                    cv: ValueCommitment(AffinePoint::identity()),
                    nullifier,
                    rk: redjubjub::VerificationKeyBytes::from(rpk_bytes),
                    zkproof: proof,
                    spend_auth_sig: redjubjub::Signature::from({
                        let mut b = [0u8; 64];
                        b.copy_from_slice(sig_bytes.as_slice());
                        b
                    }),
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Spend<SharedAnchor> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<note::Nullifier>(),
            array::uniform32(any::<u8>()),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(nullifier, rpk_bytes, proof, sig_bytes)| Self {
                per_spend_anchor: FieldNotPresent,
                cv: ValueCommitment(AffinePoint::identity()),
                nullifier,
                rk: redjubjub::VerificationKeyBytes::from(rpk_bytes),
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
                cv: ValueCommitment(AffinePoint::identity()),
                cm_u: NoteCommitment(AffinePoint::identity()).extract_u(),
                ephemeral_key: keys::EphemeralPublicKey(AffinePoint::identity()),
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
