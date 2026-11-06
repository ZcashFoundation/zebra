//! Tests for the `get-hashes` entry and body encodings.

use proptest::prelude::*;

use crate::{
    block::{
        self,
        sync_metadata::{read_sync_hashes_body, write_sync_hashes_body},
        SyncHashEntry,
    },
    serialization::{SerializationError, ZcashDeserializeInto, ZcashSerialize},
};

/// An entry whose counts hit each CompactSize width boundary.
fn boundary_entry() -> SyncHashEntry {
    SyncHashEntry {
        hash: block::Hash([0xAB; 32]),
        size: 0xFC,
        txs: 0xFD,
        txouts: 0x1_0000,
        notes: 0x1_0000_0000,
    }
}

fn body(entries: &[SyncHashEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_sync_hashes_body(entries, &mut bytes).expect("writing to a Vec succeeds");
    bytes
}

#[test]
fn entry_encoding_vector() {
    let _init_guard = zebra_test::init();

    let mut expected = vec![0xAB; 32];
    // size 0xFC: one byte.
    expected.push(0xFC);
    // txs 0xFD: the 0xFD marker and a u16.
    expected.extend([0xFD, 0xFD, 0x00]);
    // txouts 0x1_0000: the 0xFE marker and a u32.
    expected.extend([0xFE, 0x00, 0x00, 0x01, 0x00]);
    // notes 0x1_0000_0000: the 0xFF marker and a u64.
    expected.extend([0xFF, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);

    let entry = boundary_entry();
    let bytes = entry
        .zcash_serialize_to_vec()
        .expect("writing to a Vec succeeds");
    assert_eq!(bytes, expected);

    let read: SyncHashEntry = bytes
        .as_slice()
        .zcash_deserialize_into()
        .expect("valid entry");
    assert_eq!(read, entry);
}

#[test]
fn body_includes_the_count() {
    let _init_guard = zebra_test::init();

    // An empty body is just the zero count.
    assert_eq!(body(&[]), vec![0x00]);
    assert_eq!(read_sync_hashes_body(&[0x00], 0).expect("valid"), vec![]);

    let entry = boundary_entry();
    let mut expected = vec![0x02];
    for _ in 0..2 {
        expected.extend(entry.zcash_serialize_to_vec().expect("Vec"));
    }
    assert_eq!(body(&[entry, entry]), expected);
    assert_eq!(
        read_sync_hashes_body(&expected, 2).expect("valid"),
        vec![entry, entry]
    );
}

#[test]
fn body_count_bound() {
    let _init_guard = zebra_test::init();

    let entry = boundary_entry();
    let bytes = body(&[entry, entry, entry]);

    assert_eq!(read_sync_hashes_body(&bytes, 3).expect("valid").len(), 3);
    assert!(matches!(
        read_sync_hashes_body(&bytes, 2),
        Err(SerializationError::Parse(_))
    ));

    // A huge claimed count is rejected from the count alone.
    let mut huge = vec![0xFE];
    huge.extend(u32::MAX.to_le_bytes());
    assert!(matches!(
        read_sync_hashes_body(&huge, 50_000),
        Err(SerializationError::Parse(_))
    ));

    // A count within the bound, but with too few entries, is truncated.
    let mut short = vec![0xFD];
    short.extend(50_000u16.to_le_bytes());
    assert!(matches!(
        read_sync_hashes_body(&short, 50_000),
        Err(SerializationError::Io(_))
    ));
}

#[test]
fn body_rejects_truncation_and_trailing_bytes() {
    let _init_guard = zebra_test::init();

    let bytes = body(&[boundary_entry()]);

    for len in 0..bytes.len() {
        assert!(
            read_sync_hashes_body(&bytes[..len], 1).is_err(),
            "a {len}-byte prefix of a {}-byte body is truncated",
            bytes.len(),
        );
    }

    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        read_sync_hashes_body(&trailing, 1),
        Err(SerializationError::Parse(_))
    ));
}

#[test]
fn non_canonical_compact_sizes_are_rejected() {
    let _init_guard = zebra_test::init();

    // Each of these encodes a value that fits in a shorter form.
    let non_canonical: [&[u8]; 3] = [
        &[0xFD, 0xFC, 0x00],
        &[0xFE, 0xFF, 0xFF, 0x00, 0x00],
        &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00],
    ];

    for encoding in non_canonical {
        // In each of an entry's four count fields.
        for field in 0..4 {
            let mut entry = vec![0x11; 32];
            for other in 0..4 {
                if other == field {
                    entry.extend(encoding);
                } else {
                    entry.push(0x01);
                }
            }
            let result: Result<SyncHashEntry, _> = entry.as_slice().zcash_deserialize_into();
            assert!(
                matches!(result, Err(SerializationError::Parse(_))),
                "field {field} with {encoding:02x?}: {result:?}",
            );

            let mut bytes = vec![0x01];
            bytes.extend(&entry);
            assert!(read_sync_hashes_body(&bytes, 1).is_err());
        }

        // And in the body's count, which is checked before any entry is read.
        assert!(matches!(
            read_sync_hashes_body(encoding, u32::MAX),
            Err(SerializationError::Parse(_))
        ));
    }
}

proptest! {
    #[test]
    fn entry_roundtrip(entry in any::<SyncHashEntry>()) {
        let _init_guard = zebra_test::init();

        let bytes = entry.zcash_serialize_to_vec()?;
        prop_assert!((32 + 4..=32 + 4 * 9).contains(&bytes.len()));

        let read: SyncHashEntry = bytes.as_slice().zcash_deserialize_into()?;
        prop_assert_eq!(read, entry);
    }

    #[test]
    fn body_roundtrip(entries in proptest::collection::vec(any::<SyncHashEntry>(), 0..64)) {
        let _init_guard = zebra_test::init();

        let bytes = body(&entries);
        // `entries.len()` is below 64, so it fits in a `u32`.
        let len = entries.len() as u32;

        prop_assert_eq!(&read_sync_hashes_body(&bytes, len)?, &entries);

        if let Some(max) = len.checked_sub(1) {
            prop_assert!(read_sync_hashes_body(&bytes, max).is_err());
        }

        // Re-encoding the decoded entries gives the same bytes.
        prop_assert_eq!(body(&read_sync_hashes_body(&bytes, len)?), bytes);
    }
}
