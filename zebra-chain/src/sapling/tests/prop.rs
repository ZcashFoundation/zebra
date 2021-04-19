use proptest::prelude::*;

use crate::{
    block,
    sapling::{self, PerSpendAnchor, SharedAnchor},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{LockTime, Transaction},
};

use futures::future::Either;
use sapling::OutputInTransactionV4;

proptest! {
    /// Serialize and deserialize `Spend<PerSpendAnchor>`
    #[test]
    fn spend_v4_roundtrip(
        spend in any::<sapling::Spend<PerSpendAnchor>>(),
    ) {
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
    fn shielded_data_v4_roundtrip(
        shielded_v4 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
    ) {
        zebra_test::init();

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction

        // stick `PerSpendAnchor` shielded data into a v4 transaction
        let tx = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
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
    fn shielded_data_v5_roundtrip(
        shielded_v5 in any::<sapling::ShieldedData<SharedAnchor>>(),
    ) {
        zebra_test::init();

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
    fn shielded_data_v4_outputs_only(
        shielded_v4 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
    ) {
        use Either::*;

        zebra_test::init();

        // we need at least one output to delete all the spends
        prop_assume!(shielded_v4.outputs().count() > 0);

        // TODO: modify the strategy, rather than the shielded data
        let mut shielded_v4 = shielded_v4;
        let mut outputs: Vec<_> = shielded_v4.outputs().cloned().collect();
        shielded_v4.rest_spends = Vec::new();
        shielded_v4.first = Right(outputs.remove(0));
        shielded_v4.rest_outputs = outputs;

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction

        // stick `PerSpendAnchor` shielded data into a v4 transaction
        let tx = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
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
    fn shielded_data_v5_outputs_only(
        shielded_v5 in any::<sapling::ShieldedData<SharedAnchor>>(),
    ) {
        use Either::*;

        zebra_test::init();

        // we need at least one output to delete all the spends
        prop_assume!(shielded_v5.outputs().count() > 0);

        // TODO: modify the strategy, rather than the shielded data
        let mut shielded_v5 = shielded_v5;
        let mut outputs: Vec<_> = shielded_v5.outputs().cloned().collect();
        shielded_v5.rest_spends = Vec::new();
        shielded_v5.first = Right(outputs.remove(0));
        shielded_v5.rest_outputs = outputs;
        // TODO: delete the shared anchor when there are no spends
        shielded_v5.shared_anchor = Default::default();

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

    /// Check that ShieldedData<PerSpendAnchor> is equal when `first` is swapped
    /// between a spend and an output
    #[test]
    fn shielded_data_per_spend_swap_first_eq(
        shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>()
    ) {
        use Either::*;

        zebra_test::init();

        // we need at least one spend and one output to swap them
        prop_assume!(shielded1.spends().count() > 0 && shielded1.outputs().count() > 0);

        let mut shielded2 = shielded1.clone();
        let mut spends: Vec<_> = shielded2.spends().cloned().collect();
        let mut outputs: Vec<_> = shielded2.outputs().cloned().collect();
        match shielded2.first {
            Left(_spend) => {
                shielded2.first = Right(outputs.remove(0));
                shielded2.rest_outputs = outputs;
                shielded2.rest_spends = spends;
            }
            Right(_output) => {
                shielded2.first = Left(spends.remove(0));
                shielded2.rest_spends = spends;
                shielded2.rest_outputs = outputs;
            }
        }

        prop_assert_eq![&shielded1, &shielded2];

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction
        let tx1 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded1),
        };
        let tx2 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded2),
        };

        prop_assert_eq![&tx1, &tx2];

        let data1 = tx1.zcash_serialize_to_vec().expect("tx1 should serialize");
        let data2 = tx2.zcash_serialize_to_vec().expect("tx2 should serialize");

        prop_assert_eq![data1, data2];
    }

    /// Check that ShieldedData<SharedAnchor> is equal when `first` is swapped
    /// between a spend and an output
    #[test]
    fn shielded_data_shared_swap_first_eq(
        shielded1 in any::<sapling::ShieldedData<SharedAnchor>>()
    ) {
        use Either::*;

        zebra_test::init();

        // we need at least one spend and one output to swap them
        prop_assume!(shielded1.spends().count() > 0 && shielded1.outputs().count() > 0);

        let mut shielded2 = shielded1.clone();
        let mut spends: Vec<_> = shielded2.spends().cloned().collect();
        let mut outputs: Vec<_> = shielded2.outputs().cloned().collect();
        match shielded2.first {
            Left(_spend) => {
                shielded2.first = Right(outputs.remove(0));
                shielded2.rest_outputs = outputs;
                shielded2.rest_spends = spends;
            }
            Right(_output) => {
                shielded2.first = Left(spends.remove(0));
                shielded2.rest_spends = spends;
                shielded2.rest_outputs = outputs;
            }
        }

        prop_assert_eq![&shielded1, &shielded2];

        let data1 = shielded1.zcash_serialize_to_vec().expect("shielded1 should serialize");
        let data2 = shielded2.zcash_serialize_to_vec().expect("shielded2 should serialize");

        prop_assert_eq![data1, data2];
    }

    /// Check that ShieldedData<PerSpendAnchor> serialization is equal if
    /// `shielded1 == shielded2`
    #[test]
    fn shielded_data_per_spend_serialize_eq(
        shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
        shielded2 in any::<sapling::ShieldedData<PerSpendAnchor>>()
    ) {
        zebra_test::init();

        let shielded_eq = shielded1 == shielded2;

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction
        let tx1 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded1),
        };
        let tx2 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded2),
        };

        if shielded_eq {
            prop_assert_eq![&tx1, &tx2];
        } else {
            prop_assert_ne![&tx1, &tx2];
        }

        let data1 = tx1.zcash_serialize_to_vec().expect("tx1 should serialize");
        let data2 = tx2.zcash_serialize_to_vec().expect("tx2 should serialize");

        if shielded_eq {
            prop_assert_eq![data1, data2];
        } else {
            prop_assert_ne![data1, data2];
        }
    }

    /// Check that ShieldedData<SharedAnchor> serialization is equal if
    /// `shielded1 == shielded2`
    #[test]
    fn shielded_data_shared_serialize_eq(
        shielded1 in any::<sapling::ShieldedData<SharedAnchor>>(),
        shielded2 in any::<sapling::ShieldedData<SharedAnchor>>()
    ) {
        zebra_test::init();

        let shielded_eq = shielded1 == shielded2;

        let data1 = shielded1.zcash_serialize_to_vec().expect("shielded1 should serialize");
        let data2 = shielded2.zcash_serialize_to_vec().expect("shielded2 should serialize");

        if shielded_eq {
            prop_assert_eq![data1, data2];
        } else {
            prop_assert_ne![data1, data2];
        }
    }

    /// Check that ShieldedData<PerSpendAnchor> serialization is equal when we
    /// replace all the known fields.
    ///
    /// This test checks for extra fields that are not in `ShieldedData::eq`.
    #[test]
    fn shielded_data_per_spend_field_assign_eq(
        shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>(),
        shielded2 in any::<sapling::ShieldedData<PerSpendAnchor>>()
    ) {
        zebra_test::init();

        let mut shielded2 = shielded2;

        // these fields must match ShieldedData::eq
        // the spends() and outputs() checks cover first, rest_spends, and rest_outputs
        shielded2.first = shielded1.first.clone();
        shielded2.rest_spends = shielded1.rest_spends.clone();
        shielded2.rest_outputs = shielded1.rest_outputs.clone();
        // now for the fields that are checked literally
        shielded2.value_balance = shielded1.value_balance;
        shielded2.shared_anchor = shielded1.shared_anchor;
        shielded2.binding_sig = shielded1.binding_sig;

        prop_assert_eq![&shielded1, &shielded2];

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction
        let tx1 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded1),
        };
        let tx2 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded2),
        };

        prop_assert_eq![&tx1, &tx2];

        let data1 = tx1.zcash_serialize_to_vec().expect("tx1 should serialize");
        let data2 = tx2.zcash_serialize_to_vec().expect("tx2 should serialize");

        prop_assert_eq![data1, data2];
    }

    /// Check that ShieldedData<SharedAnchor> serialization is equal when we
    /// replace all the known fields.
    ///
    /// This test checks for extra fields that are not in `ShieldedData::eq`.
    #[test]
    fn shielded_data_shared_field_assign_eq(
        shielded1 in any::<sapling::ShieldedData<SharedAnchor>>(),
        shielded2 in any::<sapling::ShieldedData<SharedAnchor>>()
    ) {
        zebra_test::init();

        let mut shielded2 = shielded2;

        // TODO: modify the strategy, rather than the shielded data
        //
        // these fields must match ShieldedData::eq
        // the spends() and outputs() checks cover first, rest_spends, and rest_outputs
        shielded2.first = shielded1.first.clone();
        shielded2.rest_spends = shielded1.rest_spends.clone();
        shielded2.rest_outputs = shielded1.rest_outputs.clone();
        // now for the fields that are checked literally
        shielded2.value_balance = shielded1.value_balance;
        shielded2.shared_anchor = shielded1.shared_anchor;
        shielded2.binding_sig = shielded1.binding_sig;

        prop_assert_eq![&shielded1, &shielded2];

        let data1 = shielded1.zcash_serialize_to_vec().expect("shielded1 should serialize");
        let data2 = shielded2.zcash_serialize_to_vec().expect("shielded2 should serialize");

        prop_assert_eq![data1, data2];
    }
}
