//! Property-based tests for basic serialization primitives.

use proptest::prelude::*;

use std::io::Cursor;

use crate::serialization::{ReadZcashExt, WriteZcashExt};

proptest! {
    #[test]
    fn compactsize_write_then_read_round_trip(s in 0u64..0x2_0000u64) {
        zebra_test::init();

        // Maximum encoding size of a compactsize is 9 bytes.
        let mut buf = [0u8; 8+1];
        Cursor::new(&mut buf[..]).write_compactsize(s).unwrap();
        let expect_s = Cursor::new(&buf[..]).read_compactsize().unwrap();
        prop_assert_eq!(s, expect_s);
    }

    #[test]
    fn compactsize_read_then_write_round_trip(bytes in prop::array::uniform9(0u8..)) {
        zebra_test::init();

        // Only do the test if the bytes were valid.
        if let Ok(s) = Cursor::new(&bytes[..]).read_compactsize() {
            // The compactsize encoding is variable-length, so we may not even
            // read all of the input bytes, and therefore we can't expect that
            // the encoding will reproduce bytes that were never read. Instead,
            // copy the input bytes, and overwrite them with the encoding of s,
            // so that if the encoding is different, we'll catch it on the part
            // that's written.
            let mut expect_bytes = bytes;
            Cursor::new(&mut expect_bytes[..]).write_compactsize(s).unwrap();
            prop_assert_eq!(bytes, expect_bytes);
        }
    }
}
