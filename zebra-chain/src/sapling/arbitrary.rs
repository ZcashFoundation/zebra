use std::convert::{TryFrom, TryInto};

use group::ff::PrimeField;
use jubjub::AffinePoint;
use proptest::{arbitrary::any, collection::vec, prelude::*};

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
            any::<SpendVerificationKeyBytesWrapper>(),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(per_spend_anchor, nullifier, rk, proof, sig_bytes)| Self {
                per_spend_anchor,
                cv: ValueCommitment(AffinePoint::identity()),
                nullifier,
                rk: rk.0,
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
            any::<SpendVerificationKeyBytesWrapper>(),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(nullifier, rk, proof, sig_bytes)| Self {
                per_spend_anchor: FieldNotPresent,
                cv: ValueCommitment(AffinePoint::identity()),
                nullifier,
                rk: rk.0,
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

/// A wrapper for a RedJubJub spending verification key.
///
/// This is used by proptests since we can't implement Arbitrary for the
/// redjubjub::VerificationKeyBytes (it's in another crate).
/// We then implement it for the wrapper type instead.
#[derive(Debug)]
struct SpendVerificationKeyBytesWrapper(redjubjub::VerificationKeyBytes<redjubjub::SpendAuth>);

impl Arbitrary for SpendVerificationKeyBytesWrapper {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 32))
            .prop_filter_map("invalid verification key", |bytes| {
                let bytes: [u8; 32] = bytes.try_into().expect("vec is the correct length");
                let vkb = redjubjub::VerificationKeyBytes::<redjubjub::SpendAuth>::try_from(bytes)
                    .expect("a valid generated verification key bytes");
                // Convert to a VerificationKey to make sure it's valid;
                // but return the underlying bytes if it works.
                let r = redjubjub::VerificationKey::<redjubjub::SpendAuth>::try_from(vkb);
                r.ok().map(|_| SpendVerificationKeyBytesWrapper(vkb))
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for tree::Root {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 32))
            .prop_filter_map("invalid Sapling nore commitment tree root", |bytes| {
                let bytes = bytes.try_into().expect("vec is the correct length");
                // Convert to a Scalar to make sure it's valid;
                // but return the underlying bytes if it works.
                let r = bls12_381::Scalar::from_repr(bytes);
                r.map(|_| Self(bytes))
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
