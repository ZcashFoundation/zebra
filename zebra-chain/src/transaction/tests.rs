use proptest::{arbitrary::any, collection::vec, option, prelude::*};

use crate::{
    serialization::{DeserializeInto, ZcashDeserialize, ZcashSerialize},
    types::LockTime,
};

use super::*;

mod arbitrary;
mod test_vectors;

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

#[test]
fn librustzcash_tx_deserialize_and_round_trip() {
    let tx = Transaction::zcash_deserialize(&test_vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&test_vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

proptest! {

    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx2];
    }
}
