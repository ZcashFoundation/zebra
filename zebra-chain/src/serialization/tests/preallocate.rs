//! Tests for trusted preallocation during deserialization.

use proptest::{collection::size_range, prelude::*};

use std::matches;

use crate::serialization::{
    zcash_deserialize::MAX_U8_ALLOCATION, SerializationError, ZcashDeserialize, ZcashSerialize,
    MAX_PROTOCOL_MESSAGE_LEN,
};

// Allow direct serialization of Vec<u8> for these tests. We don't usually
// allow this because some types have specific rules for about serialization
// of their inner Vec<u8>. This method could be easily misused if it applied
// more generally.
impl ZcashSerialize for u8 {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&[*self])
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

#[test]
/// Confirm that every u8 takes exactly 1 byte when serialized.
/// This verifies that our calculated `MAX_U8_ALLOCATION` is indeed an upper bound.
fn u8_size_is_correct() {
    for byte in std::u8::MIN..=std::u8::MAX {
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
    let mut shortest_disallowed_vec = vec![0u8; MAX_U8_ALLOCATION + 1];
    let shortest_disallowed_serialized = shortest_disallowed_vec
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");

    // Confirm that shortest_disallowed_vec is only one item larger than the limit
    assert_eq!((shortest_disallowed_vec.len() - 1), MAX_U8_ALLOCATION);
    // Confirm that shortest_disallowed_vec is too large to be included in a valid zcash message
    assert!(shortest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

    // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
    shortest_disallowed_vec.pop();
    let longest_allowed_vec = shortest_disallowed_vec;
    let longest_allowed_serialized = longest_allowed_vec
        .zcash_serialize_to_vec()
        .expect("serialization to vec must succed");

    // Check that our largest_allowed_vec contains the maximum number of items
    assert_eq!(longest_allowed_vec.len(), MAX_U8_ALLOCATION);
    // Check that our largest_allowed_vec is the size of a maximal protocol message
    assert_eq!(longest_allowed_serialized.len(), MAX_PROTOCOL_MESSAGE_LEN);
}
