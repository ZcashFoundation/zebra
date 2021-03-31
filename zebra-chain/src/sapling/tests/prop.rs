use proptest::prelude::*;

use super::super::super::transaction::*;
use super::super::shielded_data::*;

use crate::{
    block,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

proptest! {
    #[test]
    fn shielded_data_roundtrip(shielded in any::<ShieldedData<PerSpendAnchor>>()) {
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

    /// Check that ShieldedData serialization is equal if `shielded1 == shielded2`
    #[test]
    fn shielded_data_serialize_eq(shielded1 in any::<ShieldedData<PerSpendAnchor>>(), shielded2 in any::<ShieldedData<PerSpendAnchor>>()) {
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

        let data1 = tx1.zcash_serialize_to_vec().expect("tx1 should serialize");
        let data2 = tx2.zcash_serialize_to_vec().expect("tx2 should serialize");

        if shielded_eq {
            prop_assert_eq![data1, data2];
        } else {
            prop_assert_ne![data1, data2];
        }
    }
}
