use proptest::{arbitrary::any, array, collection::vec, prelude::*};

use crate::primitives::Groth16Proof;

use super::super::{commitment, keys, note, tree, Output, Spend};

impl Arbitrary for Spend {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<tree::SaplingNoteTreeRootHash>(),
            any::<commitment::ValueCommitment>(),
            any::<note::Nullifier>(),
            array::uniform32(any::<u8>()),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(
                |(anchor, cv, nullifier, rpk_bytes, proof, sig_bytes)| Self {
                    anchor,
                    cv,
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

impl Arbitrary for Output {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<commitment::ValueCommitment>(),
            any::<commitment::NoteCommitment>(),
            any::<keys::EphemeralPublicKey>(),
            any::<note::EncryptedCiphertext>(),
            any::<note::OutCiphertext>(),
            any::<Groth16Proof>(),
        )
            .prop_map(
                |(cv, cm, ephemeral_key, enc_ciphertext, out_ciphertext, zkproof)| Self {
                    cv,
                    cm_u: cm.extract_u(),
                    ephemeral_key,
                    enc_ciphertext,
                    out_ciphertext,
                    zkproof,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
