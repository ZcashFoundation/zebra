//! Property-based tests for basic serialization primitives.

use std::io::Cursor;

use proptest::prelude::*;

use crate::{
    serialization::{
        CompactSize64, CompactSizeMessage, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::UnminedTx,
};

proptest! {
    #[test]
    fn compact_size_message_write_then_read_round_trip(size in any::<CompactSizeMessage>()) {
        let _init_guard = zebra_test::init();

        let buf = size.zcash_serialize_to_vec().unwrap();
        // Maximum encoding size of a CompactSize is 9 bytes.
        prop_assert!(buf.len() <= 9);

        let expect_size: CompactSizeMessage = buf.zcash_deserialize_into().unwrap();
        prop_assert_eq!(size, expect_size);

        // Also check the range is correct
        let size: usize = size.into();
        prop_assert!(size <= MAX_PROTOCOL_MESSAGE_LEN);
    }

    #[test]
    fn compact_size_message_read_then_write_round_trip(bytes in prop::array::uniform9(0u8..)) {
        let _init_guard = zebra_test::init();

        // Only do the test if the bytes were valid.
        if let Ok(size) = CompactSizeMessage::zcash_deserialize(Cursor::new(&bytes[..])) {
            // The CompactSize encoding is variable-length, so we may not even
            // read all of the input bytes, and therefore we can't expect that
            // the encoding will reproduce bytes that were never read. Instead,
            // copy the input bytes, and overwrite them with the encoding of s,
            // so that if the encoding is different, we'll catch it on the part
            // that's written.
            let mut expect_bytes = bytes;
            size.zcash_serialize(Cursor::new(&mut expect_bytes[..])).unwrap();

            prop_assert_eq!(bytes, expect_bytes);

            // Also check the range is correct
            let size: usize = size.into();
            prop_assert!(size <= MAX_PROTOCOL_MESSAGE_LEN);
        }
    }

    #[test]
    fn compact_size_message_range(size in any::<usize>()) {
        let _init_guard = zebra_test::init();

        if let Ok(compact_size) = CompactSizeMessage::try_from(size) {
            prop_assert!(size <= MAX_PROTOCOL_MESSAGE_LEN);

            let compact_size: usize = compact_size.into();
            prop_assert_eq!(size, compact_size);
        } else {
            prop_assert!(size > MAX_PROTOCOL_MESSAGE_LEN);
        }
    }

    #[test]
    fn compact_size_64_write_then_read_round_trip(size in any::<CompactSize64>()) {
        let _init_guard = zebra_test::init();

        let buf = size.zcash_serialize_to_vec().unwrap();
        // Maximum encoding size of a CompactSize is 9 bytes.
        prop_assert!(buf.len() <= 9);

        let expect_size: CompactSize64 = buf.zcash_deserialize_into().unwrap();
        prop_assert_eq!(size, expect_size);
    }

    #[test]
    fn compact_size_64_read_then_write_round_trip(bytes in prop::array::uniform9(0u8..)) {
        let _init_guard = zebra_test::init();

        // Only do the test if the bytes were valid.
        if let Ok(s) = CompactSize64::zcash_deserialize(Cursor::new(&bytes[..])) {
            // The CompactSize encoding is variable-length, so we may not even
            // read all of the input bytes, and therefore we can't expect that
            // the encoding will reproduce bytes that were never read. Instead,
            // copy the input bytes, and overwrite them with the encoding of s,
            // so that if the encoding is different, we'll catch it on the part
            // that's written.
            let mut expect_bytes = bytes;
            s.zcash_serialize(Cursor::new(&mut expect_bytes[..])).unwrap();

            prop_assert_eq!(bytes, expect_bytes);
        }
    }

    #[test]
    fn compact_size_64_conversion(size in any::<u64>()) {
        let _init_guard = zebra_test::init();

        let compact_size = CompactSize64::from(size);
        let compact_size: u64 = compact_size.into();
        prop_assert_eq!(size, compact_size);
    }

    #[test]
    fn transaction_serialized_size(transaction in any::<UnminedTx>()) {
        let _init_guard = zebra_test::init();

        prop_assert_eq!(transaction.transaction.zcash_serialized_size(), transaction.size);
    }
}
