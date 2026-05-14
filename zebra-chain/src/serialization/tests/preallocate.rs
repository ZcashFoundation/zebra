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
/// Capping the initial allocation must not break correctness for legitimately
/// large vectors: the `Vec` should still grow via `push()` to hold all
/// elements when the body is complete. This complements
/// `external_count_above_initial_cap_panics_pre_fix` below by confirming the
/// cap path produces correct results for ordinary valid messages.
#[test]
fn external_count_above_initial_cap_still_deserializes() {
    use crate::block;

    let count = 4 * 1024;
    let body = vec![0u8; count * 32];

    let deserialized: Vec<block::Hash> = zcash_deserialize_external_count(count, &body[..])
        .expect("a full body should deserialize regardless of the initial allocation cap");

    assert_eq!(deserialized.len(), count);
}

/// A test-only wrapper around `u8` with an essentially unbounded
/// `TrustedPreallocate::max_allocation()`. Used to drive
/// `zcash_deserialize_external_count` with a count large enough that the
/// pre-fix `Vec::with_capacity(external_count)` allocation would panic with
/// "capacity overflow" (the requested byte size exceeds `isize::MAX`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct UnboundedTestByte(u8);

impl ZcashDeserialize for UnboundedTestByte {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;
        Ok(UnboundedTestByte(buf[0]))
    }
}

impl TrustedPreallocate for UnboundedTestByte {
    fn max_allocation() -> u64 {
        // Intentionally unbounded for this test — we want the function to
        // accept the count and reach the (now capped) allocation site.
        u64::MAX
    }
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10545.
///
/// On `origin/main`, `zcash_deserialize_external_count` did
/// `Vec::with_capacity(external_count)` before reading any element bytes. For
/// a count whose byte size exceeds `isize::MAX`, this panics with "capacity
/// overflow" (Rust's documented `Vec::with_capacity` precondition). After the
/// fix, the initial capacity is capped at `MAX_INITIAL_ALLOCATION = 1024`, so
/// the function reaches the reader instead and surfaces the truncated body as
/// a clean I/O error.
///
/// This test calls the helper with an external count well above
/// `isize::MAX` and a short body. It returns `Err(SerializationError::Io)`
/// with the fix in place; without the fix the call panics inside
/// `Vec::with_capacity` before it gets to the reader.
#[test]
fn external_count_above_isize_max_returns_io_error_instead_of_panicking() {
    use std::io::Cursor;

    // `Vec::with_capacity(N)` for non-ZST `T` requires `N * size_of::<T>() <=
    // isize::MAX`. `UnboundedTestByte` is 1 byte, so any `N > isize::MAX as
    // usize` would trigger capacity overflow on the pre-fix path.
    let panic_inducing_count: usize = (isize::MAX as usize) + 1;
    let body: Vec<u8> = vec![0u8; 16];

    let result: Result<Vec<UnboundedTestByte>, _> =
        zcash_deserialize_external_count(panic_inducing_count, Cursor::new(body));

    assert!(
        matches!(result, Err(SerializationError::Io(_))),
        "with the cap in place the call surfaces a truncated-body I/O error \
         rather than panicking inside `Vec::with_capacity`; got {result:?}"
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
