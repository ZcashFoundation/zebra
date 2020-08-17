use proptest::prelude::*;
use std::io::Cursor;

use super::super::*;

use crate::serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize};

proptest! {
    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![tx, tx2];
    }

    #[test]
    fn locktime_roundtrip(locktime in any::<LockTime>()) {
        let mut bytes = Cursor::new(Vec::new());
        locktime.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_locktime = LockTime::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![locktime, other_locktime];
    }
}
