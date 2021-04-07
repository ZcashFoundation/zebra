use proptest::prelude::*;

use crate::{
    block,
    sapling::{self, PerSpendAnchor},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{LockTime, Transaction},
};

use futures::future::Either;

proptest! {
    // TODO: generalise this test for `ShieldedData<SharedAnchor>` (#1829)
    #[test]
    fn shielded_data_roundtrip(shielded in any::<sapling::ShieldedData<PerSpendAnchor>>()) {
        zebra_test::init();

        // shielded data doesn't serialize by itself, so we have to stick it in
        // a transaction
        let tx = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: block::Height(0),
            joinsplit_data: None,
            sapling_shielded_data: Some(shielded),
        };

        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx_parsed = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx_parsed];
    }

    /// Check that ShieldedData<PerSpendAnchor> is equal when `first` is swapped
    /// between a spend and an output
    //
    // TODO: generalise this test for `ShieldedData<SharedAnchor>` (#1829)
    #[test]
    fn shielded_data_per_spend_swap_first_eq(shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>()) {
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

    /// Check that ShieldedData<PerSpendAnchor> serialization is equal if
    /// `shielded1 == shielded2`
    //
    // TODO: generalise this test for `ShieldedData<SharedAnchor>` (#1829)
    #[test]
    fn shielded_data_per_spend_serialize_eq(shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>(), shielded2 in any::<sapling::ShieldedData<PerSpendAnchor>>()) {
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

    /// Check that ShieldedData<PerSpendAnchor> serialization is equal when we
    /// replace all the known fields.
    ///
    /// This test checks for extra fields that are not in `ShieldedData::eq`.
    //
    // TODO: generalise this test for `ShieldedData<SharedAnchor>` (#1829)
    #[test]
    fn shielded_data_per_spend_field_assign_eq(shielded1 in any::<sapling::ShieldedData<PerSpendAnchor>>(), shielded2 in any::<sapling::ShieldedData<PerSpendAnchor>>()) {
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
}
