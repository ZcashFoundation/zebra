//! Randomised data generation for sapling types.

use group::Group;
use jubjub::ExtendedPoint;
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

use proptest::{collection::vec, prelude::*};

use crate::primitives::Groth16Proof;

use super::{
    keys::ValidatingKey, note, tree, FieldNotPresent, Output, OutputInTransactionV4,
    PerSpendAnchor, SharedAnchor, Spend,
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
                cv: ExtendedPoint::generator().into(),
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
                cv: ExtendedPoint::generator().into(),
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
                cv: ExtendedPoint::generator().into(),
                cm_u: sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&[0u8; 32])
                    .unwrap(),
                ephemeral_key: jubjub::AffinePoint::from(ExtendedPoint::generator()).into(),
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
        let rng = ChaChaRng::from_seed(bytes);
        let sk = redjubjub::SigningKey::<redjubjub::SpendAuth>::new(rng);
        redjubjub::VerificationKey::<redjubjub::SpendAuth>::from(&sk)
            .try_into()
            .unwrap()
    })
}

fn jubjub_base_strat() -> BoxedStrategy<jubjub::Base> {
    (vec(any::<u8>(), 64))
        .prop_map(|bytes| {
            let bytes = bytes.try_into().expect("vec is the correct length");
            jubjub::Base::from_bytes_wide(&bytes)
        })
        .boxed()
}

impl Arbitrary for tree::Root {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        jubjub_base_strat().prop_map(tree::Root).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for tree::legacy::Node {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        jubjub_base_strat()
            .prop_map(tree::legacy::Node::from)
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl From<jubjub::Fq> for tree::legacy::Node {
    fn from(x: jubjub::Fq) -> Self {
        let node = sapling_crypto::Node::from_bytes(x.to_bytes());
        if node.is_some().into() {
            tree::legacy::Node(node.unwrap())
        } else {
            sapling_crypto::Node::from_bytes([0; 32]).unwrap().into()
        }
    }
}
