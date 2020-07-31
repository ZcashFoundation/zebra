use proptest::{arbitrary::any, collection::vec, option, prelude::*};

use crate::{
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    types::LockTime,
};

use super::*;

mod arbitrary;

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
    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

proptest! {

    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx2];
    }
}

#[test]
fn zip143_deserialize_and_round_trip() {
    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_1[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_2[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_2[..], &data2[..]);
}

#[test]
fn zip243_deserialize_and_round_trip() {
    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_1[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_2[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_2[..], &data2[..]);

    let tx3 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_3[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data3 = Vec::new();
    tx3.zcash_serialize(&mut data3)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_3[..], &data3[..]);
}
