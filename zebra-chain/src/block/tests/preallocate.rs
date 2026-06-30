//! Tests for trusted preallocation during deserialization.

use std::sync::Arc;

use proptest::prelude::*;

use crate::{
    block::{CountedHeader, Hash, Header, MAX_BLOCK_LOCATOR_LENGTH},
    serialization::{
        arbitrary::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize,
        MAX_HEADERS_PER_MESSAGE, MAX_PROTOCOL_MESSAGE_LEN,
    },
};

/// The serialized size of a Zcash block header.
///
/// Equihash input + 32-byte nonce + 3-byte equihash solution-length field +
/// equihash solution. Used as a serialized-size lower bound by the
/// `counted_header_min_length` proptest.
const BLOCK_HEADER_LENGTH: usize =
    crate::work::equihash::Solution::INPUT_LENGTH + 32 + 3 + crate::work::equihash::SOLUTION_SIZE;

/// The minimum size for a serialized `CountedHeader`: header bytes plus at
/// least one byte for the transaction count CompactSize.
const MIN_COUNTED_HEADER_LEN: usize = BLOCK_HEADER_LENGTH + 1;

proptest! {
    /// Verify that the serialized size of a block hash is 32 bytes, matching the protocol spec.
    #[test]
    fn block_hash_size_is_correct(hash in Hash::arbitrary()) {
        let serialized = hash.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == 32);
    }

    /// Verify that...
    /// 1. `Hash::max_allocation` is exactly `MAX_BLOCK_LOCATOR_LENGTH`. The cap is intentionally
    ///    far smaller than the message-size limit, to prevent peer-controlled preallocation
    ///    amplification (sibling of GHSA-xr93-pcq3-pxf8).
    /// 2. The largest allowed vector still fits in a legal Zcash Wire Protocol message.
    #[test]
    fn block_hash_max_allocation(hash in Hash::arbitrary_with(())) {
        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(hash);

        // The cap is exactly the locator-protocol cap, not derived from message size.
        prop_assert!(Hash::max_allocation() == MAX_BLOCK_LOCATOR_LENGTH);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Hash::max_allocation());

        // Check that our largest_allowed_vec contains the maximum number of hashes
        prop_assert!((largest_allowed_vec_len as u64) == Hash::max_allocation());

        // Check that our largest_allowed_vec is small enough to send as a protocol message
        // (this is now slack: the locator cap is much smaller than the message-size limit).
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
    /// 1. `CountedHeader::max_allocation` is exactly `MAX_HEADERS_PER_MESSAGE`. The cap is a
    ///    protocol-level constant (160 headers per `headers` message — the Zcash convention
    ///    Zebra already enforces on the sending side and at the codec level for `read_headers`)
    ///    rather than a message-size-derived value, to prevent peer-controlled preallocation
    ///    amplification (sibling of GHSA-xr93-pcq3-pxf8).
    /// 2. The largest allowed vector still fits in a legal Zcash Wire Protocol message.
    #[test]
    fn counted_header_max_allocation(header in any::<Arc<Header>>()) {
        let header = CountedHeader {
            header,
        };

        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(header);

        // The cap is exactly the headers-protocol cap, not derived from message size.
        // Cast safe: MAX_HEADERS_PER_MESSAGE is the constant 160 (well under u64::MAX).
        prop_assert!(CountedHeader::max_allocation() == MAX_HEADERS_PER_MESSAGE as u64);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == CountedHeader::max_allocation());

        // Check that our largest_allowed_vec contains the maximum number of CountedHeaders
        prop_assert!((largest_allowed_vec_len as u64) == CountedHeader::max_allocation());

        // Check that our largest_allowed_vec is small enough to send as a protocol message
        // (this is now slack: the headers cap is much smaller than the message-size limit).
        prop_assert!(largest_allowed_serialized_len <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
