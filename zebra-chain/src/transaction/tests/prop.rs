use proptest::prelude::*;

use super::super::*;

use crate::serialization::{ZcashDeserializeInto, ZcashSerialize};

proptest! {

    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx2];
    }
}
