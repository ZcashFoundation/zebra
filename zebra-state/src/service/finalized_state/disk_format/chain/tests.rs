//! Chain-format serialization tests.

use super::*;

/// A history tree written by a pre-NU6.3 database format (253-byte entries) must be read in
/// place, zero-padding each entry up to the current width.
#[test]
fn history_tree_parts_reads_legacy_entry_width() {
    // Two legacy-width entries with distinct, nonzero content.
    let legacy = LegacyHistoryTreeParts {
        network_kind: NetworkKind::Mainnet,
        size: 42,
        peaks: BTreeMap::from([
            (
                0,
                LegacyEntry {
                    inner: [0xAB; LEGACY_MAX_ENTRY_SIZE],
                },
            ),
            (
                5,
                LegacyEntry {
                    inner: [0xCD; LEGACY_MAX_ENTRY_SIZE],
                },
            ),
        ]),
        current_height: Height(1_000),
    };

    let legacy_bytes = bincode::DefaultOptions::new()
        .serialize(&legacy)
        .expect("legacy serialization succeeds");

    // Reading the narrower blob must succeed via the compatibility fallback (this used to panic
    // with `UnexpectedEof` because the current, wider entry width overran the stored bytes).
    let parts = HistoryTreeParts::from_bytes(&legacy_bytes);

    assert_eq!(parts.network_kind, NetworkKind::Mainnet);
    assert_eq!(parts.size, 42);
    assert_eq!(parts.current_height, Height(1_000));
    assert_eq!(parts.peaks.len(), 2);

    // The read path reconstructs exactly the same parts as converting the legacy data directly,
    // and re-encodes at the current (wider) entry width.
    assert_eq!(parts.as_bytes(), HistoryTreeParts::from(legacy).as_bytes());
    assert!(parts.as_bytes().len() > legacy_bytes.len());
}

/// A multi-peak legacy history tree whose bytes make the current-width parse fail with a *non-EOF*
/// bincode error must still fall back to the legacy width.
///
/// The `0xFF` fill is deliberate: entries are fixed-size arrays with no length prefix, so parsing
/// the first 253-byte entry at the current (wider) width overruns into the next map key, which
/// bincode decodes from `0xFF` bytes and rejects as an invalid varint — not `UnexpectedEof`. The
/// previous fallback gated on `UnexpectedEof` only, so it panicked on records like this.
#[test]
fn history_tree_parts_reads_legacy_entry_width_with_non_eof_misparse() {
    let legacy = LegacyHistoryTreeParts {
        network_kind: NetworkKind::Mainnet,
        size: 7,
        peaks: BTreeMap::from([
            (
                0,
                LegacyEntry {
                    inner: [0xFF; LEGACY_MAX_ENTRY_SIZE],
                },
            ),
            (
                1,
                LegacyEntry {
                    inner: [0xFF; LEGACY_MAX_ENTRY_SIZE],
                },
            ),
        ]),
        current_height: Height(500),
    };

    let legacy_bytes = bincode::DefaultOptions::new()
        .serialize(&legacy)
        .expect("legacy serialization succeeds");

    // Sanity check: the current-width parse of this record fails with a non-`UnexpectedEof` error —
    // the exact case the old fallback did not cover.
    let current_err = bincode::DefaultOptions::new()
        .deserialize::<HistoryTreeParts>(&legacy_bytes)
        .err()
        .expect("current-width parse of a legacy record fails");
    assert!(
        !matches!(
            current_err.as_ref(),
            bincode::ErrorKind::Io(io_err) if io_err.kind() == std::io::ErrorKind::UnexpectedEof,
        ),
        "expected a non-EOF misparse error, got: {current_err:?}",
    );

    // The fallback reads it anyway, instead of panicking.
    let parts = HistoryTreeParts::from_bytes(&legacy_bytes);
    assert_eq!(parts.network_kind, NetworkKind::Mainnet);
    assert_eq!(parts.size, 7);
    assert_eq!(parts.current_height, Height(500));
    assert_eq!(parts.peaks.len(), 2);
    assert_eq!(parts.as_bytes(), HistoryTreeParts::from(legacy).as_bytes());
}

/// Data written at the current entry width round-trips without hitting the legacy fallback.
#[test]
fn history_tree_parts_round_trips_current_width() {
    let parts = HistoryTreeParts {
        network_kind: NetworkKind::Testnet,
        size: 3,
        peaks: BTreeMap::from([(
            0,
            zcash_history::Entry::from_raw_bytes_padded(&[7; LEGACY_MAX_ENTRY_SIZE]),
        )]),
        current_height: Height(9),
    };

    let bytes = parts.as_bytes();
    let parsed = HistoryTreeParts::from_bytes(&bytes);

    assert_eq!(parsed.network_kind, NetworkKind::Testnet);
    assert_eq!(parsed.size, 3);
    assert_eq!(parsed.current_height, Height(9));
    assert_eq!(parsed.as_bytes(), bytes);
}

/// A synchronization metadata record round-trips through its 64-byte disk
/// format, with every field at its documented offset, and reading tolerates
/// unknown trailing bytes.
#[test]
fn sync_metadata_round_trips_with_cumulative_output_count() {
    let metadata = SyncMetadata {
        size: 0x0102_0304,
        tx_count: 5,
        note_count: 6,
        sapling_tx_count: 7,
        orchard_tx_count: 8,
        ironwood_tx_count: 9,
        auth_data_root: [0xAA; 32],
        cumulative_transparent_outputs: 0x1122_3344_5566_7788,
    };

    let bytes = metadata.as_bytes();
    assert_eq!(bytes.len(), 64, "records are 64 bytes");

    let mut expected = Vec::new();
    expected.extend_from_slice(&[0x04, 0x03, 0x02, 0x01]);
    for count in 5u32..=9 {
        expected.extend_from_slice(&count.to_le_bytes());
    }
    expected.extend_from_slice(&[0xAA; 32]);
    expected.extend_from_slice(&[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    assert_eq!(
        bytes.as_slice(),
        expected.as_slice(),
        "the layout is little-endian"
    );

    assert_eq!(SyncMetadata::from_bytes(bytes), metadata);

    // Unknown appended fields are ignored when reading.
    let mut extended = bytes.to_vec();
    extended.extend_from_slice(&[0xFF; 8]);
    assert_eq!(SyncMetadata::from_bytes(&extended), metadata);
}

/// The snapshot base marker round-trips through its fixed-width disk encoding.
#[test]
fn snapshot_base_round_trips_through_disk_format() {
    let _init_guard = zebra_test::init();

    let base = SnapshotBase {
        window_start: Height(3_519_757),
        height: Height(3_522_060),
        block_hash: block::Hash([0x11; 32]),
        manifest: ObjectHash([0x22; 32]),
        deferred_checks: SnapshotBase::DEFERRED_BLOCK_HASH
            | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS,
    };

    let bytes = base.as_bytes();
    assert_eq!(bytes.len(), SNAPSHOT_BASE_DISK_BYTES);
    // Heights are 3-byte big-endian, as everywhere else in the database.
    assert_eq!(&bytes[..3], Height(3_519_757).as_bytes());
    assert_eq!(&bytes[3..6], Height(3_522_060).as_bytes());
    assert_eq!(&bytes[6..38], [0x11; 32]);
    assert_eq!(&bytes[38..70], [0x22; 32]);
    assert_eq!(bytes[70], 0b11);

    assert_eq!(SnapshotBase::from_bytes(bytes), base);
    assert!(base.has_deferred(SnapshotBase::DEFERRED_BLOCK_HASH));
    assert!(!SnapshotBase {
        deferred_checks: 0,
        ..base
    }
    .has_deferred(SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS));
}
