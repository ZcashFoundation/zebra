//! Tests for trusted preallocation during deserialization.

use crate::{
    block::{
        header::MIN_COUNTED_HEADER_LEN, CountedHeader, Hash, Header, BLOCK_HASH_SIZE,
        MAX_BLOCK_BYTES, MAX_PROTOCOL_MESSAGE_LEN,
    },
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::prelude::*;
use std::convert::TryInto;

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
        let max_allocation: usize = Hash::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(Hash::max_allocation()+1) {
            smallest_disallowed_vec.push(hash);
        }

        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Hash::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        prop_assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of hashes
        prop_assert!((largest_allowed_vec.len() as u64) == Hash::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);

    }

    /// Confirm that each counted header takes at least COUNTED_HEADER_LEN bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn counted_header_min_length(header in Header::arbitrary_with(()), transaction_count in (0..MAX_BLOCK_BYTES)) {
        let header = CountedHeader {
            header,
            transaction_count: transaction_count.try_into().expect("Must run test on platform with at least 32 bit address space"),
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
    fn counted_header_max_allocation(header in Header::arbitrary_with(())) {
        let header = CountedHeader {
            header,
            transaction_count: 0,
        };
        let max_allocation: usize = CountedHeader::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(CountedHeader::max_allocation()+1) {
            smallest_disallowed_vec.push(header.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == CountedHeader::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        prop_assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);


        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of CountedHeaders
        prop_assert!((largest_allowed_vec.len() as u64) == CountedHeader::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
