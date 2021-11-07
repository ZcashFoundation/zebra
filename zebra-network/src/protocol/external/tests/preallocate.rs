//! Tests for trusted preallocation during deserialization.

use std::convert::TryInto;

use proptest::prelude::*;

use zebra_chain::serialization::{TrustedPreallocate, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN};

use crate::meta_addr::MetaAddr;

use super::super::{
    addr::{AddrV1, ADDR_V1_SIZE},
    inv::InventoryHash,
};

proptest! {
    /// Confirm that each InventoryHash takes the expected size in bytes when serialized.
    #[test]
    fn inv_hash_size_is_correct(inv in any::<InventoryHash>()) {
        let serialized_inv = inv
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        let expected_size = match inv {
            InventoryHash::Error
            | InventoryHash::Tx(_)
            | InventoryHash::Block(_)
            | InventoryHash::FilteredBlock(_) => 32 + 4,

            InventoryHash::Wtx(_) => 32 + 32 + 4,
        };

        assert_eq!(serialized_inv.len(), expected_size);
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

proptest! {
    /// Confirm that each AddrV1 takes exactly ADDR_V1_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn addr_v1_size_is_correct(addr in MetaAddr::arbitrary()) {
        zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize();
        prop_assume!(addr.is_some());

        let addr: AddrV1 = addr.unwrap().into();

        let serialized = addr
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized.len() == ADDR_V1_SIZE)
    }

    /// Verifies that...
    /// 1. The smallest disallowed vector of `AddrV1`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    #[test]
    fn addr_v1_max_allocation_is_correct(addr in MetaAddr::arbitrary()) {
        zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize();
        prop_assume!(addr.is_some());

        let addr: AddrV1 = addr.unwrap().into();

        let max_allocation: usize = AddrV1::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(AddrV1::max_allocation() + 1) {
            smallest_disallowed_vec.push(addr);
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        assert!(((smallest_disallowed_vec.len() - 1) as u64) == AddrV1::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send in a valid Zcash message
        assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of AddrV1s
        assert!((largest_allowed_vec.len() as u64) == AddrV1::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash message.
        assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
