use crate::{
    commitments, keys,
    notes::{sapling, sprout},
    proofs::{Groth16Proof, ZkSnarkProof},
    transaction::{
        CoinbaseData, JoinSplit, JoinSplitData, OutPoint, Output, ShieldedData, Spend, Transaction,
        TransparentInput,
    },
    treestate::{self, note_commitment_tree::SaplingNoteTreeRootHash},
    types::{
        amount::{Amount, NonNegative},
        BlockHeight, Script,
    },
};
use futures::future::Either;
use proptest::{array, collection::vec, prelude::*};

impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplit<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<treestate::sprout::NoteTreeRootHash>(),
            array::uniform2(any::<sprout::Nullifier>()),
            array::uniform2(any::<commitments::sprout::NoteCommitment>()),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            array::uniform2(any::<crate::types::MAC>()),
            any::<P>(),
            array::uniform2(any::<sprout::EncryptedCiphertext>()),
        )
            .prop_map(
                |(
                    vpub_old,
                    vpub_new,
                    anchor,
                    nullifiers,
                    commitments,
                    ephemeral_key_bytes,
                    random_seed,
                    vmacs,
                    zkproof,
                    enc_ciphertexts,
                )| {
                    Self {
                        vpub_old,
                        vpub_new,
                        anchor,
                        nullifiers,
                        commitments,
                        ephemeral_key: x25519_dalek::PublicKey::from(ephemeral_key_bytes),
                        random_seed,
                        vmacs,
                        zkproof,
                        enc_ciphertexts,
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplitData<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<JoinSplit<P>>(),
            vec(any::<JoinSplit<P>>(), 0..10),
            array::uniform32(any::<u8>()),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest, pub_key_bytes, sig_bytes)| Self {
                first,
                rest,
                pub_key: ed25519_zebra::VerificationKeyBytes::from(pub_key_bytes),
                sig: ed25519_zebra::Signature::from({
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
            any::<commitments::sapling::ValueCommitment>(),
            any::<commitments::sapling::NoteCommitment>(),
            any::<keys::sapling::EphemeralPublicKey>(),
            any::<sapling::EncryptedCiphertext>(),
            any::<sapling::OutCiphertext>(),
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
            any::<SaplingNoteTreeRootHash>(),
            any::<commitments::sapling::ValueCommitment>(),
            any::<sapling::Nullifier>(),
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
                .prop_map(|(outpoint, unlock_script, sequence)| {
                    TransparentInput::PrevOut {
                        outpoint,
                        unlock_script,
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
