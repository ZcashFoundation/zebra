//! Tests for trusted preallocation during deserialization.

use std::sync::Arc;

use proptest::prelude::*;

use crate::{
    block::{
        header::MIN_COUNTED_HEADER_LEN, CountedHeader, Hash, Header, BLOCK_HASH_SIZE,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
    serialization::{arbitrary::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize},
};

proptest! {
    /// Verify that the serialized size of a block hash used to calculate the allocation limit is correct
    #[test]
    fn block_hash_size_is_correct(hash in Hash::arbitrary()) {
        let serialized = hash.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == BLOCK_HASH_SIZE);
    }

    /// Verify that...
    /// 1. The smallest disallowed vector of `Hash`s is too large to send via the Zcash Wire Protocol
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash Wire Protocol message
    #[test]
    fn block_hash_max_allocation(hash in Hash::arbitrary_with(())) {
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(hash);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Hash::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        prop_assert!(smallest_disallowed_serialized_len > MAX_PROTOCOL_MESSAGE_LEN);

        // Check that our largest_allowed_vec contains the maximum number of hashes
        prop_assert!((largest_allowed_vec_len as u64) == Hash::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!(largest_allowed_serialized_len <= MAX_PROTOCOL_MESSAGE_LEN);
    }

    /// Confirm that each counted header takes at least COUNTED_HEADER_LEN bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn counted_header_min_length(header in any::<Arc<Header>>()) {
        let header = CountedHeader {
            header,
        };
        let serialized_header = header.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized_header.len() >= MIN_COUNTED_HEADER_LEN)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `CountedHeaders`s is too large to send via the Zcash Wire Protocol
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash Wire Protocol message
    #[test]
    fn counted_header_max_allocation(header in any::<Arc<Header>>()) {
        let header = CountedHeader {
            header,
        };

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(header);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == CountedHeader::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        prop_assert!(smallest_disallowed_serialized_len > MAX_PROTOCOL_MESSAGE_LEN);

        // Check that our largest_allowed_vec contains the maximum number of CountedHeaders
        prop_assert!((largest_allowed_vec_len as u64) == CountedHeader::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!(largest_allowed_serialized_len <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
