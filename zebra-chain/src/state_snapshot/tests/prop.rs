//! Randomised round trips for state snapshot commitments.

use proptest::prelude::*;

use crate::{
    block,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

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

    /// Every commitment round trips through serde, with its block hash in
    /// display order.
    #[test]
    fn commitment_round_trips(
        height in 0..=block::Height::MAX.0,
        block_hash in any::<[u8; 32]>(),
        manifest in any::<ObjectHash>(),
    ) {
        let _init_guard = zebra_test::init();

        let commitment = StateSnapshotCommitment {
            height: block::Height(height),
            block_hash: block::Hash(block_hash),
            manifest,
        };

        let json = serde_json::to_value(commitment).expect("serializes");
        prop_assert_eq!(&json["hash"], &serde_json::json!(commitment.block_hash.to_string()));
        prop_assert_eq!(&json["manifest"], &serde_json::json!(manifest.to_string()));
        prop_assert_eq!(
            serde_json::from_value::<StateSnapshotCommitment>(json).expect("deserializes"),
            commitment,
        );
    }
}
