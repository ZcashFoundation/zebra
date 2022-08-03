//! Randomised property tests for amounts.

use proptest::prelude::*;

use crate::amount::*;

proptest! {
    #[test]
    fn amount_serialization(amount in any::<Amount<NegativeAllowed>>()) {
        let _init_guard = zebra_test::init();

        let bytes = amount.to_bytes();
        let serialized_amount = Amount::<NegativeAllowed>::from_bytes(bytes)?;

        prop_assert_eq!(amount, serialized_amount);
    }

    #[test]
    fn amount_deserialization(bytes in any::<[u8; 8]>()) {
        let _init_guard = zebra_test::init();

        if let Ok(deserialized) = Amount::<NegativeAllowed>::from_bytes(bytes) {
            let bytes2 = deserialized.to_bytes();
            prop_assert_eq!(bytes, bytes2);
        }
    }
}
