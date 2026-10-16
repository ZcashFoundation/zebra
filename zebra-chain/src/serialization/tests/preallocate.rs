//! Tests for trusted preallocation during deserialization.

use proptest::{collection::size_range, prelude::*};

use std::matches;

use crate::serialization::{
    arbitrary::max_allocation_is_big_enough,
    zcash_deserialize::{
        zcash_deserialize_bytes_external_count, zcash_deserialize_string_external_count,
        MAX_INITIAL_ALLOCATION, MAX_U8_ALLOCATION,
    },
    SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    MAX_PROTOCOL_MESSAGE_LEN,
};

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

#[test]
/// A peer-supplied byte count at the `MAX_U8_ALLOCATION` ceiling, paired with a
/// near-empty reader, must fail closed with `UnexpectedEof` rather than
/// pre-allocating the full claimed count.
fn bytes_external_count_rejects_short_reader() {
    let data = [0u8; 4];
    let result =
        zcash_deserialize_bytes_external_count(MAX_U8_ALLOCATION, std::io::Cursor::new(&data[..]));

    match result {
        Err(SerializationError::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof)
        }
        other => panic!("expected an UnexpectedEof io error, got {other:?}"),
    }
}

#[test]
/// Round-trip a byte vector several times larger than `MAX_INITIAL_ALLOCATION`,
/// so the buffer grows past the initial reservation while all bytes are present.
fn bytes_external_count_roundtrips_past_initial_allocation() {
    let len = MAX_INITIAL_ALLOCATION * 2 + 7;
    let input: Vec<u8> = (0..len).map(|i| i as u8).collect();
    let deserialized = zcash_deserialize_bytes_external_count(len, std::io::Cursor::new(&input))
        .expect("deserialization must succeed when all bytes are present");

    assert_eq!(deserialized, input);
}

#[test]
/// Confirm the `String` specialisation inherits the same bound and fail-closed
/// behaviour, since it delegates to `zcash_deserialize_bytes_external_count`.
fn string_external_count_rejects_short_reader() {
    let data = [0u8; 4];
    let result =
        zcash_deserialize_string_external_count(MAX_U8_ALLOCATION, std::io::Cursor::new(&data[..]));

    match result {
        Err(SerializationError::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof)
        }
        other => panic!("expected an UnexpectedEof io error, got {other:?}"),
    }
}
