//! Tests for trusted preallocation during deserialization.

use std::{cmp::min, env};

use proptest::prelude::*;

use zebra_chain::{
    parameters::Network::*,
    serialization::{
        arbitrary::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{
        addr::{AddrV1, AddrV2, ADDR_V1_SIZE, ADDR_V2_MIN_SIZE},
        inv::{InventoryHash, MAX_INV_IN_RECEIVED_MESSAGE},
    },
};

/// The number of test cases to use for expensive proptests.
const DEFAULT_PROPTEST_CASES: u32 = 8;

proptest! {
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                                                 .ok()
                                                                 .and_then(|v| v.parse().ok())
                                                                 .unwrap_or(DEFAULT_PROPTEST_CASES)))]

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

        prop_assert_eq!(serialized_inv.len(), expected_size);
    }

    /// Verifies that...
    /// 1. The smallest disallowed vector of `InventoryHash`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    #[test]
    fn inv_hash_max_allocation_is_correct(inv in InventoryHash::smallest_types_strategy()) {
        let max_allocation: usize = InventoryHash::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = vec![inv; max_allocation + 1];

        let smallest_disallowed_serialized = smallest_disallowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == InventoryHash::max_allocation());
        // Check that our smallest_disallowed_vec is too big to fit in a Zcash message.
        //
        // Special case: Zcash has a slightly smaller limit for transaction invs,
        // so we use it for all invs.
        prop_assert!(smallest_disallowed_serialized.len() > min(MAX_PROTOCOL_MESSAGE_LEN, usize::try_from(MAX_INV_IN_RECEIVED_MESSAGE).expect("fits in usize")));

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of InventoryHashes
        prop_assert!((largest_allowed_vec.len() as u64) == InventoryHash::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash message.
        prop_assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}

proptest! {
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                                                 .ok()
                                                                 .and_then(|v| v.parse().ok())
                                                                 .unwrap_or(DEFAULT_PROPTEST_CASES)))]

    /// Confirm that each AddrV1 takes exactly ADDR_V1_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn addr_v1_size_is_correct(addr in MetaAddr::arbitrary()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize(Mainnet);
        prop_assume!(addr.is_some());

        let addr: AddrV1 = addr.unwrap().into();

        let serialized = addr
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() == ADDR_V1_SIZE)
    }

    /// Verifies that...
    /// 1. The smallest disallowed vector of `AddrV1`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    #[test]
    fn addr_v1_max_allocation_is_correct(addr in MetaAddr::arbitrary()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize(Mainnet);
        prop_assume!(addr.is_some());

        let addr: AddrV1 = addr.unwrap().into();

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(addr);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == AddrV1::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send in a valid Zcash message
        prop_assert!(smallest_disallowed_serialized_len > MAX_PROTOCOL_MESSAGE_LEN);

        // Check that our largest_allowed_vec contains the maximum number of AddrV1s
        prop_assert!((largest_allowed_vec_len as u64) == AddrV1::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash message.
        prop_assert!(largest_allowed_serialized_len <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}

proptest! {
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                                                 .ok()
                                                                 .and_then(|v| v.parse().ok())
                                                                 .unwrap_or(DEFAULT_PROPTEST_CASES)))]

    /// Confirm that each AddrV2 takes at least ADDR_V2_MIN_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn addr_v2_size_is_correct(addr in MetaAddr::arbitrary()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize(Mainnet);
        prop_assume!(addr.is_some());

        let addr: AddrV2 = addr.unwrap().into();

        let serialized = addr
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() >= ADDR_V2_MIN_SIZE)
    }

    /// Verifies that...
    /// 1. The smallest disallowed vector of `AddrV2`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    #[test]
    fn addr_v2_max_allocation_is_correct(addr in MetaAddr::arbitrary()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization
        let addr = addr.sanitize(Mainnet);
        prop_assume!(addr.is_some());

        let addr: AddrV2 = addr.unwrap().into();

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            _largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(addr);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == AddrV2::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send in a valid Zcash message
        prop_assert!(smallest_disallowed_serialized_len > MAX_PROTOCOL_MESSAGE_LEN);

        // Check that our largest_allowed_vec contains the maximum number of AddrV2s
        prop_assert!((largest_allowed_vec_len as u64) == AddrV2::max_allocation());
        // This is a variable-sized type, so largest_allowed_serialized_len can exceed the length limit
    }
}
