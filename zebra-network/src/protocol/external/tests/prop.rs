use proptest::prelude::*;

use zebra_chain::serialization::{ZcashDeserializeInto, ZcashSerialize};

use super::super::InventoryHash;

proptest! {
    /// Test if [`InventoryHash`] is not changed after serializing and deserializing it.
    #[test]
    fn inventory_hash_roundtrip(inventory_hash in any::<InventoryHash>()) {
        let mut bytes = Vec::new();

        let serialization_result = inventory_hash.zcash_serialize(&mut bytes);

        prop_assert!(serialization_result.is_ok());

        let deserialized: Result<InventoryHash, _> = bytes.zcash_deserialize_into();

        prop_assert!(deserialized.is_ok());
        prop_assert_eq!(deserialized.unwrap(), inventory_hash);
    }
}
