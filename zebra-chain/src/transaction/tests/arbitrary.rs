use crate::{
    note_commitment_tree::SaplingNoteTreeRootHash,
    notes::sapling,
    proofs::Groth16Proof,
    transaction::{
        CoinbaseData, OutPoint, Output, ShieldedData, Spend, Transaction, TransparentInput,
    },
    types::{BlockHeight, Script},
};
use futures::future::Either;
use proptest::{array, collection::vec, prelude::*};

impl Arbitrary for Output {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()).prop_filter("Valid jubjub::AffinePoint", |b| {
                jubjub::AffinePoint::from_bytes(*b).is_some().unwrap_u8() == 1
            }),
            any::<sapling::EncryptedCiphertext>(),
            any::<sapling::OutCiphertext>(),
            any::<Groth16Proof>(),
        )
            .prop_map(
                |(cv, cmu, ephemeral_key_bytes, enc_ciphertext, out_ciphertext, zkproof)| Self {
                    cv,
                    cmu,
                    ephemeral_key: jubjub::AffinePoint::from_bytes(ephemeral_key_bytes).unwrap(),
                    enc_ciphertext,
                    out_ciphertext,
                    zkproof,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for ShieldedData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            prop_oneof![
                any::<Spend>().prop_map(Either::Left),
                any::<Output>().prop_map(Either::Right)
            ],
            vec(any::<Spend>(), 0..10),
            vec(any::<Output>(), 0..10),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest_spends, rest_outputs, sig_bytes)| Self {
                first,
                rest_spends,
                rest_outputs,
                binding_sig: redjubjub::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Spend {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            array::uniform32(any::<u8>()),
            any::<SaplingNoteTreeRootHash>(),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(
                |(cv_bytes, anchor, nullifier_bytes, rpk_bytes, proof, sig_bytes)| Self {
                    anchor,
                    cv: cv_bytes,
                    nullifier: nullifier_bytes,
                    rk: redjubjub::PublicKeyBytes::from(rpk_bytes),
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

impl Arbitrary for Transaction {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            Self::v1_strategy(),
            Self::v2_strategy(),
            Self::v3_strategy(),
            Self::v4_strategy()
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for TransparentInput {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (any::<OutPoint>(), any::<Script>(), any::<u32>())
                .prop_map(|(outpoint, script, sequence)| {
                    TransparentInput::PrevOut {
                        outpoint,
                        script,
                        sequence,
                    }
                })
                .boxed(),
            (any::<BlockHeight>(), vec(any::<u8>(), 0..95), any::<u32>())
                .prop_map(|(height, data, sequence)| {
                    TransparentInput::Coinbase {
                        height,
                        data: CoinbaseData(data),
                        sequence,
                    }
                })
                .boxed(),
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
