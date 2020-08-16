use chrono::{TimeZone, Utc};
use futures::future::Either;
use proptest::{arbitrary::any, array, collection::vec, option, prelude::*};

use crate::{
    amount::{Amount, NonNegative},
    block::BlockHeight,
    commitments,
    notes::sprout,
    primitives::{Bctv14Proof, Groth16Proof, Script, ZkSnarkProof},
    sapling,
    transaction::{
        CoinbaseData, JoinSplit, JoinSplitData, LockTime, OutPoint, Output, ShieldedData, Spend,
        Transaction, TransparentInput, TransparentOutput,
    },
    treestate,
};

impl Transaction {
    pub fn v1_strategy() -> impl Strategy<Value = Self> {
        (
            vec(any::<TransparentInput>(), 0..10),
            vec(any::<TransparentOutput>(), 0..10),
            any::<LockTime>(),
        )
            .prop_map(|(inputs, outputs, lock_time)| Transaction::V1 {
                inputs,
                outputs,
                lock_time,
            })
            .boxed()
    }

    pub fn v2_strategy() -> impl Strategy<Value = Self> {
        (
            vec(any::<TransparentInput>(), 0..10),
            vec(any::<TransparentOutput>(), 0..10),
            any::<LockTime>(),
            option::of(any::<JoinSplitData<Bctv14Proof>>()),
        )
            .prop_map(
                |(inputs, outputs, lock_time, joinsplit_data)| Transaction::V2 {
                    inputs,
                    outputs,
                    lock_time,
                    joinsplit_data,
                },
            )
            .boxed()
    }

    pub fn v3_strategy() -> impl Strategy<Value = Self> {
        (
            vec(any::<TransparentInput>(), 0..10),
            vec(any::<TransparentOutput>(), 0..10),
            any::<LockTime>(),
            any::<BlockHeight>(),
            option::of(any::<JoinSplitData<Bctv14Proof>>()),
        )
            .prop_map(
                |(inputs, outputs, lock_time, expiry_height, joinsplit_data)| Transaction::V3 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    joinsplit_data,
                },
            )
            .boxed()
    }

    pub fn v4_strategy() -> impl Strategy<Value = Self> {
        (
            vec(any::<TransparentInput>(), 0..10),
            vec(any::<TransparentOutput>(), 0..10),
            any::<LockTime>(),
            any::<BlockHeight>(),
            any::<Amount>(),
            option::of(any::<ShieldedData>()),
            option::of(any::<JoinSplitData<Groth16Proof>>()),
        )
            .prop_map(
                |(
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
                )| Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
                },
            )
            .boxed()
    }
}

impl Arbitrary for LockTime {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        prop_oneof![
            (BlockHeight::MIN.0..=BlockHeight::MAX.0)
                .prop_map(|n| LockTime::Height(BlockHeight(n))),
            (LockTime::MIN_TIMESTAMP..=LockTime::MAX_TIMESTAMP)
                .prop_map(|n| { LockTime::Time(Utc.timestamp(n as i64, 0)) })
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

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
            any::<sapling::commitment::ValueCommitment>(),
            any::<sapling::commitment::NoteCommitment>(),
            any::<sapling::keys::EphemeralPublicKey>(),
            any::<sapling::note::EncryptedCiphertext>(),
            any::<sapling::note::OutCiphertext>(),
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
            any::<sapling::tree::SaplingNoteTreeRootHash>(),
            any::<sapling::commitment::ValueCommitment>(),
            any::<sapling::note::Nullifier>(),
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
