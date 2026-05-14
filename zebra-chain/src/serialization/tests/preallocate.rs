//! Tests for trusted preallocation during deserialization.

use proptest::{collection::size_range, prelude::*};

use std::matches;

use crate::serialization::{
    arbitrary::max_allocation_is_big_enough,
    zcash_deserialize::{zcash_deserialize_external_count, MAX_U8_ALLOCATION},
    SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    MAX_PROTOCOL_MESSAGE_LEN,
};

// Allow direct serialization of Vec<u8> for these tests. We don't usually
// allow this because some types have specific rules for about serialization
// of their inner Vec<u8>. This method could be easily misused if it applied
// more generally.
//
// Due to Rust's trait rules, these trait impls apply to all zebra-chain tests,
// not just the tests in this module. But other crates' tests can't access them.
impl ZcashSerialize for u8 {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&[*self])
    }
}

impl TrustedPreallocate for u8 {
    fn max_allocation() -> u64 {
        // MAX_PROTOCOL_MESSAGE_LEN takes up 5 bytes when encoded as a CompactSize.
        (MAX_PROTOCOL_MESSAGE_LEN - 5)
            .try_into()
            .expect("MAX_PROTOCOL_MESSAGE_LEN fits in u64")
    }
}

proptest! {
#![proptest_config(ProptestConfig::with_cases(4))]

    #[test]
    /// Confirm that deserialize yields the expected result for any vec smaller than `MAX_U8_ALLOCATION`
    fn u8_ser_deser_roundtrip(input in any_with::<Vec<u8>>(size_range(MAX_U8_ALLOCATION).lift()) ) {
        let serialized = input.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        let cursor = std::io::Cursor::new(serialized);
        let deserialized = <Vec<u8>>::zcash_deserialize(cursor).expect("deserialization from vec must succeed");
        prop_assert_eq!(deserialized, input)
    }
}

#[test]
/// Confirm that deserialize allows vectors with length up to and including `MAX_U8_ALLOCATION`
fn u8_deser_accepts_max_valid_input() {
    let serialized = vec![0u8; MAX_U8_ALLOCATION]
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");
    let cursor = std::io::Cursor::new(serialized);
    let deserialized = <Vec<u8>>::zcash_deserialize(cursor);
    assert!(deserialized.is_ok())
}

#[test]
/// Confirm that rejects vectors longer than `MAX_U8_ALLOCATION`
fn u8_deser_throws_when_input_too_large() {
    let serialized = vec![0u8; MAX_U8_ALLOCATION + 1]
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");
    let cursor = std::io::Cursor::new(serialized);
    let deserialized = <Vec<u8>>::zcash_deserialize(cursor);

    assert!(matches!(
        deserialized,
        Err(SerializationError::Parse(
            "Byte vector longer than MAX_U8_ALLOCATION"
        ))
    ))
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10545.
///
/// `zcash_deserialize_external_count` previously did a single
/// `Vec::with_capacity(external_count)` allocation based on the peer-supplied
/// count, before reading any element bytes. Capping the initial allocation
/// must not break correctness for legitimately large vectors: the `Vec` should
/// still grow via `push()` to hold all elements when the body is complete.
#[test]
fn external_count_above_initial_cap_still_deserializes() {
    use crate::block;

    // A count well above the initial-allocation cap (1024) but well within
    // `block::Hash::max_allocation()`. A full body of 32-byte hashes follows.
    let count = 4 * 1024;
    let body = vec![0u8; count * 32];

    let deserialized: Vec<block::Hash> = zcash_deserialize_external_count(count, &body[..])
        .expect("a full body should deserialize regardless of the initial allocation cap");

    assert_eq!(deserialized.len(), count);
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10545.
///
/// An inflated count followed by a short body must surface as a read error,
/// not as a successful giant pre-allocation. With the initial-allocation cap
/// in place, the failure path runs after a small allocation and before any
/// peer-driven amplification.
#[test]
fn external_count_with_truncated_body_errors() {
    use crate::block;
    use std::io::Cursor;

    let inflated_count: usize = block::Hash::max_allocation()
        .try_into()
        .expect("max_allocation fits in usize on supported targets");
    // Far fewer bytes than `inflated_count * 32` requires.
    let body: Vec<u8> = vec![0u8; 64];

    let result: Result<Vec<block::Hash>, _> =
        zcash_deserialize_external_count(inflated_count, Cursor::new(body));

    assert!(
        matches!(result, Err(SerializationError::Io(_))),
        "truncated body under an inflated count should fail with an I/O error"
    );
}

#[test]
/// Confirm that every u8 takes exactly 1 byte when serialized.
/// This verifies that our calculated `MAX_U8_ALLOCATION` is indeed an upper bound.
fn u8_size_is_correct() {
    for byte in u8::MIN..=u8::MAX {
        let serialized = byte
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized.len() == 1)
    }
}

#[test]
/// Verify that...
/// 1. The smallest disallowed `Vec<u8>` is too big to include in a Zcash Wire Protocol message
/// 2. The largest allowed `Vec<u8>`is exactly the size of a maximal Zcash Wire Protocol message
fn u8_max_allocation_is_correct() {
    let (
        smallest_disallowed_vec_len,
        smallest_disallowed_serialized_len,
        largest_allowed_vec_len,
        largest_allowed_serialized_len,
    ) = max_allocation_is_big_enough(0u8);

    // Confirm that shortest_disallowed_vec is only one item larger than the limit
    assert_eq!((smallest_disallowed_vec_len - 1), MAX_U8_ALLOCATION);
    // Confirm that shortest_disallowed_vec is too large to be included in a valid zcash message
    assert!(smallest_disallowed_serialized_len > MAX_PROTOCOL_MESSAGE_LEN);

    // Check that our largest_allowed_vec contains the maximum number of items
    assert_eq!(largest_allowed_vec_len, MAX_U8_ALLOCATION);
    // Check that our largest_allowed_vec is the size of a maximal protocol message
    assert_eq!(largest_allowed_serialized_len, MAX_PROTOCOL_MESSAGE_LEN);
}
