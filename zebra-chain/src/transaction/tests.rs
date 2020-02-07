use proptest::{
    arbitrary::{any, Arbitrary},
    collection::vec,
    option,
    prelude::*,
};

use crate::{
    serialization::{ZcashDeserialize, ZcashSerialize},
    types::{LockTime, Script},
};

use super::*;

#[cfg(test)]
impl Transaction {
    pub fn v1_strategy() -> impl Strategy<Value = Self> {
        (
            vec(any::<TransparentInput>(), 0..10),
            vec(any::<TransparentOutput>(), 0..10),
            any::<LockTime>(),
        )
            .prop_map(|(inputs, outputs, lock_time)| Transaction::V1 {
                inputs: inputs,
                outputs: outputs,
                lock_time: lock_time,
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
                    inputs: inputs,
                    outputs: outputs,
                    lock_time: lock_time,
                    joinsplit_data: joinsplit_data,
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
                    inputs: inputs,
                    outputs: outputs,
                    lock_time: lock_time,
                    expiry_height: expiry_height,
                    joinsplit_data: joinsplit_data,
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
            any::<i64>(),
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
                    inputs: inputs,
                    outputs: outputs,
                    lock_time: lock_time,
                    expiry_height: expiry_height,
                    value_balance: value_balance,
                    shielded_data: shielded_data,
                    joinsplit_data: joinsplit_data,
                },
            )
            .boxed()
    }
}

#[cfg(test)]
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

#[cfg(test)]
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
            (vec(any::<u8>(), 0..100), any::<u32>())
                .prop_map(|(data, sequence)| { TransparentInput::Coinbase { data, sequence } })
                .boxed(),
        ]
        .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[test]
fn librustzcash_tx_deserialize_and_round_trip() {
    let tx = Transaction::zcash_deserialize(&test_vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&test_vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

#[cfg(test)]
proptest! {

    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {

        let mut data = Vec::new();

        tx.zcash_serialize(&mut data).expect("tx should serialize");

        let tx2 = Transaction::zcash_deserialize(&data[..]).expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx2];
    }
}
