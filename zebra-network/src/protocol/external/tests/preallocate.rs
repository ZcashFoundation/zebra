//! Tests for trusted preallocation during deserialization.

use super::super::inv::{InventoryHash, MIN_INV_HASH_SIZE};

use zebra_chain::serialization::{TrustedPreallocate, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN};

use proptest::prelude::*;
use std::convert::TryInto;

proptest! {
    /// Confirm that each InventoryHash takes exactly INV_HASH_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn inv_hash_size_is_correct(inv in InventoryHash::arbitrary()) {
        let serialized_inv = inv
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized_inv.len() == MIN_INV_HASH_SIZE);
    }

    /// Verifies that...
    /// 1. The smallest disallowed vector of `InventoryHash`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    #[test]
    fn inv_hash_max_allocation_is_correct(inv in InventoryHash::smallest_types_strategy()) {
        let max_allocation: usize = InventoryHash::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(InventoryHash::max_allocation() + 1) {
            smallest_disallowed_vec.push(inv);
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        assert!(((smallest_disallowed_vec.len() - 1) as u64) == InventoryHash::max_allocation());
        // Check that our smallest_disallowed_vec is too big to fit in a Zcash message.
        assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of InventoryHashes
        assert!((largest_allowed_vec.len() as u64) == InventoryHash::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash message.
        assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
