//! Sapling prop tests.

use proptest::prelude::*;

use crate::{
    block,
    sapling::{self, OutputInTransactionV4, PerSpendAnchor, SharedAnchor},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{LockTime, Transaction},
};

proptest! {
    /// Serialize and deserialize `Spend<PerSpendAnchor>`
    #[test]
    fn spend_v4_roundtrip(
        spend in any::<sapling::Spend<PerSpendAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        let data = spend.zcash_serialize_to_vec().expect("spend should serialize");
        let spend_parsed = data.zcash_deserialize_into().expect("randomized spend should deserialize");
        prop_assert_eq![&spend, &spend_parsed];

        let data2 = spend_parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    /// Serialize and deserialize `Spend<SharedAnchor>`
    #[test]
    fn spend_v5_roundtrip(
        spend in any::<sapling::Spend<SharedAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        let (prefix, zkproof, spend_auth_sig) = spend.into_v5_parts();

        let data = prefix.zcash_serialize_to_vec().expect("spend prefix should serialize");
        let parsed = data.zcash_deserialize_into().expect("randomized spend prefix should deserialize");
        prop_assert_eq![&prefix, &parsed];

        let data2 = parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];

        let data = zkproof.zcash_serialize_to_vec().expect("spend zkproof should serialize");
        let parsed = data.zcash_deserialize_into().expect("randomized spend zkproof should deserialize");
        prop_assert_eq![&zkproof, &parsed];

        let data2 = parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];

        let data = spend_auth_sig.zcash_serialize_to_vec().expect("spend auth sig should serialize");
        let parsed = data.zcash_deserialize_into().expect("randomized spend auth sig should deserialize");
        prop_assert_eq![&spend_auth_sig, &parsed];

        let data2 = parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    /// Serialize and deserialize `Output`
    #[test]
    fn output_roundtrip(
        output in any::<sapling::Output>(),
    ) {
        let _init_guard = zebra_test::init();

        // v4 format
        let data = output.clone().into_v4().zcash_serialize_to_vec().expect("output should serialize");
        let output_parsed = data.zcash_deserialize_into::<OutputInTransactionV4>().expect("randomized output should deserialize").into_output();
        prop_assert_eq![&output, &output_parsed];

        let data2 = output_parsed
            .into_v4()
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];

        // v5 format
        let (prefix, zkproof) = output.into_v5_parts();

        let data = prefix.zcash_serialize_to_vec().expect("output prefix should serialize");
        let parsed = data.zcash_deserialize_into().expect("randomized output prefix should deserialize");
        prop_assert_eq![&prefix, &parsed];

        let data2 = parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];

        let data = zkproof.zcash_serialize_to_vec().expect("output zkproof should serialize");
        let parsed = data.zcash_deserialize_into().expect("randomized output zkproof should deserialize");
        prop_assert_eq![&zkproof, &parsed];

        let data2 = parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }
}

proptest! {
    /// Serialize and deserialize `PerSpendAnchor` shielded data by including it
    /// in a V4 transaction
    #[test]
    fn sapling_shielded_data_v4_roundtrip(
        shielded_v4 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction

        // stick `PerSpendAnchor` shielded data into a v4 transaction
        let tx = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time_timestamp(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded_v4),
        };
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx_parsed = data.zcash_deserialize_into().expect("randomized tx should deserialize");
        prop_assert_eq![&tx, &tx_parsed];

        let data2 = tx_parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    /// Serialize and deserialize `SharedAnchor` shielded data
    #[test]
    fn sapling_shielded_data_v5_roundtrip(
        shielded_v5 in any::<sapling::ShieldedData<SharedAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        let data = shielded_v5.zcash_serialize_to_vec().expect("shielded_v5 should serialize");
        let shielded_v5_parsed = data.zcash_deserialize_into().expect("randomized shielded_v5 should deserialize");

        if let Some(shielded_v5_parsed) = shielded_v5_parsed {
            prop_assert_eq![&shielded_v5,
                            &shielded_v5_parsed];

            let data2 = shielded_v5_parsed
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");
            prop_assert_eq![data, data2, "data must be equal if structs are equal"];
        } else {
            panic!("unexpected parsing error: ShieldedData should be Some(_)");
        }
    }

    /// Test v4 with empty spends, but some outputs
    #[test]
    fn sapling_shielded_data_v4_outputs_only(
        shielded_v4 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        // we need at least one output to delete all the spends
        prop_assume!(shielded_v4.outputs().count() > 0);

        // TODO: modify the strategy, rather than the shielded data
        let mut shielded_v4 = shielded_v4;
        let outputs: Vec<_> = shielded_v4.outputs().cloned().collect();
        shielded_v4.transfers = sapling::TransferData::JustOutputs {
            outputs: outputs.try_into().unwrap(),
        };

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction

        // stick `PerSpendAnchor` shielded data into a v4 transaction
        let tx = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time_timestamp(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded_v4),
        };
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx_parsed = data.zcash_deserialize_into().expect("randomized tx should deserialize");
        prop_assert_eq![&tx, &tx_parsed];

        let data2 = tx_parsed
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    /// Test the v5 shared anchor serialization condition: empty spends, but some outputs
    #[test]
    fn sapling_shielded_data_v5_outputs_only(
        shielded_v5 in any::<sapling::ShieldedData<SharedAnchor>>(),
    ) {
        let _init_guard = zebra_test::init();

        // we need at least one output to delete all the spends
        prop_assume!(shielded_v5.outputs().count() > 0);

        // TODO: modify the strategy, rather than the shielded data
        let mut shielded_v5 = shielded_v5;
        let outputs: Vec<_> = shielded_v5.outputs().cloned().collect();
        shielded_v5.transfers = sapling::TransferData::JustOutputs {
            outputs: outputs.try_into().unwrap(),
        };

        let data = shielded_v5.zcash_serialize_to_vec().expect("shielded_v5 should serialize");
        let shielded_v5_parsed = data.zcash_deserialize_into().expect("randomized shielded_v5 should deserialize");

        if let Some(shielded_v5_parsed) = shielded_v5_parsed {
            prop_assert_eq![&shielded_v5,
                            &shielded_v5_parsed];

            let data2 = shielded_v5_parsed
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");
            prop_assert_eq![data, data2, "data must be equal if structs are equal"];
        } else {
            panic!("unexpected parsing error: ShieldedData should be Some(_)");
        }
    }
}
