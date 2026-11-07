//! Randomised round trips for content-addressed synchronization artifacts.

use proptest::prelude::*;

use crate::state_snapshot::ObjectHash;

proptest! {
    /// Every hash displays as 64 lowercase hex digits that parse back to it,
    /// directly and through serde.
    #[test]
    fn object_hash_round_trips(hash in any::<ObjectHash>()) {
        let _init_guard = zebra_test::init();

        let hex = hash.to_string();
        prop_assert_eq!(hex.len(), 64);
        prop_assert!(hex.chars().all(|digit| matches!(digit, '0'..='9' | 'a'..='f')));
        prop_assert_eq!(hex.parse::<ObjectHash>(), Ok(hash));
        prop_assert_eq!(hex::encode(hash.0), hex.clone());

        let json = serde_json::to_string(&hash).expect("serializes");
        prop_assert_eq!(&json, &format!("\"{hex}\""));
        prop_assert_eq!(serde_json::from_str::<ObjectHash>(&json).expect("deserializes"), hash);
    }
}
