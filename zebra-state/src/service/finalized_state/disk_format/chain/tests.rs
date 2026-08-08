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

/// A synchronization metadata record round-trips through its 64-byte disk
/// format, and reading tolerates unknown trailing bytes.
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
    assert_eq!(SyncMetadata::from_bytes(&bytes), metadata);

    // Unknown appended fields are ignored when reading.
    let mut extended = bytes.clone();
    extended.extend_from_slice(&[0xFF; 8]);
    assert_eq!(SyncMetadata::from_bytes(&extended), metadata);
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
