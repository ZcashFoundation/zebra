//! Fixed test vectors for the state snapshot format: the manifest layout, the
//! strictness rules, and the piece records.

use std::{collections::HashSet, io::Cursor};

use zebra_chain::{block, parameters::Network, state_snapshot::ObjectHash};

use crate::{
    service::finalized_state::STATE_COLUMN_FAMILIES_IN_CODE,
    state_snapshot::format::{
        ComponentId, ComponentManifest, FormatError, Manifest, PieceManifest, PieceReader,
        PieceRecord, PieceWriter, INDEX_MODE_DEFAULT, MAGIC, MAX_MANIFEST_BYTES, MAX_PIECE_BYTES,
        SNAPSHOT_FORMAT,
    },
};

/// The length of the fixed header of a manifest without a parameters hash,
/// up to and including the component count.
const HEADER_LEN: usize = 20 + 2 + 32 + 4 + 32 + 4 + 2 + 1 + 2;

/// The length of a component entry without pieces.
const EMPTY_COMPONENT_LEN: usize = 2 + 8 + 4;

/// The length of one piece entry.
const PIECE_LEN: usize = 4 + 32;

/// The snapshot height of the test manifests.
const HEIGHT: u32 = 3_522_060;

/// The window start of the test manifests: `H - 2303`.
const WINDOW_START: u32 = HEIGHT - 2303;

/// Asserts that `$result` is an error matching `$pattern`.
macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)? $(,)?) => {{
        let error = $result.expect_err(concat!("expected an error matching ", stringify!($pattern)));
        assert!(
            matches!(error, $pattern $(if $guard)?),
            "expected {}, got {error:?}",
            stringify!($pattern)
        );
    }};
}

/// A manifest for `network` whose components are all empty.
fn empty_manifest(network: &Network) -> Manifest {
    Manifest {
        genesis_hash: network.genesis_hash(),
        height: block::Height(HEIGHT),
        block_hash: block::Hash([0x11; 32]),
        window_start: block::Height(WINDOW_START),
        db_format_major: 30,
        index_mode: INDEX_MODE_DEFAULT,
        parameters_hash: Manifest::needs_parameters_hash(network).then_some([0x55; 32]),
        components: ComponentId::ALL
            .into_iter()
            .map(|id| ComponentManifest {
                id,
                entry_count: 0,
                pieces: Vec::new(),
            })
            .collect(),
    }
}

/// A manifest for `network` with pieces in a few components.
fn populated_manifest(network: &Network) -> Manifest {
    let mut manifest = empty_manifest(network);
    let utxos = &mut manifest.components[usize::from(ComponentId::Utxos.to_u16())];
    utxos.entry_count = 1_000_000;
    utxos.pieces = vec![
        PieceManifest {
            size: MAX_PIECE_BYTES as u32,
            hash: ObjectHash([0xA1; 32]),
        },
        PieceManifest {
            size: 1,
            hash: ObjectHash([0xA2; 32]),
        },
    ];
    let pools = &mut manifest.components[usize::from(ComponentId::ValuePools.to_u16())];
    pools.entry_count = 1;
    pools.pieces = vec![PieceManifest {
        size: 57,
        hash: ObjectHash([0xB1; 32]),
    }];
    manifest
}

/// The offset of component `id`'s entry in an encoded empty manifest without
/// a parameters hash.
fn component_offset(id: ComponentId) -> usize {
    HEADER_LEN + usize::from(id.to_u16()) * EMPTY_COMPONENT_LEN
}

#[test]
fn component_ids_are_dense_and_name_state_column_families() {
    let _init_guard = zebra_test::init();

    assert_eq!(ComponentId::ALL.len(), usize::from(ComponentId::COUNT));
    assert_eq!(ComponentId::COUNT, 24);
    for (index, id) in ComponentId::ALL.into_iter().enumerate() {
        let wire_id = u16::try_from(index).expect("24 fits");
        assert_eq!(id.to_u16(), wire_id, "{id}");
        assert_eq!(ComponentId::from_u16(wire_id), Some(id));
        assert!(
            STATE_COLUMN_FAMILIES_IN_CODE.contains(&id.column_family()),
            "{id} is not a state column family",
        );
        assert_eq!(id.to_string(), id.column_family());
    }
    assert_eq!(ComponentId::from_u16(ComponentId::COUNT), None);
    assert_eq!(ComponentId::from_u16(u16::MAX), None);

    let names: HashSet<&str> = ComponentId::ALL
        .into_iter()
        .map(ComponentId::column_family)
        .collect();
    assert_eq!(
        names.len(),
        ComponentId::ALL.len(),
        "duplicate column family"
    );

    // The spending index is never shipped.
    assert!(!names.contains("tx_loc_by_spent_out_loc"));
    // Neither are the checkpoint sync column families: a snapshot-born database has no
    // synchronization metadata below its window, and never applies spentness hints.
    for name in [
        "sync_meta_by_height",
        "spentness_journal",
        "spentness_artifact",
    ] {
        assert!(
            STATE_COLUMN_FAMILIES_IN_CODE.contains(&name),
            "{name} is a state column family"
        );
        assert!(!names.contains(name), "{name} is not a snapshot component");
    }
    // Design, "Snapshot contents": the canonicalized values are the chain singletons and every
    // tree.
    let canonical: Vec<ComponentId> = ComponentId::ALL
        .into_iter()
        .filter(|id| id.is_canonicalized())
        .collect();
    assert_eq!(
        canonical,
        [
            ComponentId::BlockInfo,
            ComponentId::SproutAnchors,
            ComponentId::SaplingTree,
            ComponentId::OrchardTree,
            ComponentId::IronwoodTree,
            ComponentId::SproutTipTree,
            ComponentId::HistoryTree,
            ComponentId::ValuePools,
        ],
    );
}

#[test]
fn manifest_round_trips_on_every_network() {
    let _init_guard = zebra_test::init();

    for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
        for manifest in [empty_manifest(&network), populated_manifest(&network)] {
            let bytes = manifest.encode().expect("well-formed manifest");
            assert!(bytes.len() <= MAX_MANIFEST_BYTES);
            let decoded = Manifest::decode(&bytes, &network).expect("round trips");
            assert_eq!(decoded, manifest, "{network}");
            assert_eq!(
                decoded.component(ComponentId::Utxos),
                &manifest.components[6]
            );
        }
    }

    // A parameters hash is only carried by networks whose genesis hash does not identify them.
    assert!(!Manifest::needs_parameters_hash(&Network::Mainnet));
    assert!(!Manifest::needs_parameters_hash(
        &Network::new_default_testnet()
    ));
    assert!(Manifest::needs_parameters_hash(&Network::new_regtest(
        Default::default()
    )));

    // Decoding for the wrong network misreads the component list.
    let regtest = Network::new_regtest(Default::default());
    let mainnet_bytes = empty_manifest(&Network::Mainnet).encode().expect("valid");
    assert!(Manifest::decode(&mainnet_bytes, &regtest).is_err());
    let regtest_bytes = empty_manifest(&regtest).encode().expect("valid");
    assert!(Manifest::decode(&regtest_bytes, &Network::Mainnet).is_err());
}

#[test]
fn manifest_layout_is_pinned() {
    let _init_guard = zebra_test::init();

    let manifest = populated_manifest(&Network::Mainnet);
    let bytes = manifest.encode().expect("valid");

    let utxos = &manifest.components[6];
    let piece_count = utxos.pieces.len();
    assert_eq!(
        bytes.len(),
        HEADER_LEN + 24 * EMPTY_COMPONENT_LEN + (piece_count + 1) * PIECE_LEN,
    );

    assert_eq!(&bytes[..20], MAGIC);
    assert_eq!(&bytes[20..22], SNAPSHOT_FORMAT.to_le_bytes());
    assert_eq!(&bytes[22..54], Network::Mainnet.genesis_hash().0);
    assert_eq!(&bytes[54..58], HEIGHT.to_le_bytes());
    assert_eq!(&bytes[58..90], [0x11; 32]);
    assert_eq!(&bytes[90..94], WINDOW_START.to_le_bytes());
    assert_eq!(&bytes[94..96], 30_u16.to_le_bytes());
    assert_eq!(bytes[96], INDEX_MODE_DEFAULT);
    assert_eq!(&bytes[97..99], 24_u16.to_le_bytes());

    // Component 0 is empty.
    let offset = component_offset(ComponentId::HashByHeight);
    assert_eq!(&bytes[offset..offset + 2], 0_u16.to_le_bytes());
    assert_eq!(&bytes[offset + 2..offset + 10], 0_u64.to_le_bytes());
    assert_eq!(&bytes[offset + 10..offset + 14], 0_u32.to_le_bytes());

    // Component 6 has two pieces: size then hash, each.
    let offset = component_offset(ComponentId::Utxos);
    assert_eq!(&bytes[offset..offset + 2], 6_u16.to_le_bytes());
    assert_eq!(&bytes[offset + 2..offset + 10], 1_000_000_u64.to_le_bytes());
    assert_eq!(&bytes[offset + 10..offset + 14], 2_u32.to_le_bytes());
    let piece = offset + 14;
    assert_eq!(
        &bytes[piece..piece + 4],
        (MAX_PIECE_BYTES as u32).to_le_bytes()
    );
    assert_eq!(&bytes[piece + 4..piece + 36], [0xA1; 32]);
    assert_eq!(&bytes[piece + 36..piece + 40], 1_u32.to_le_bytes());
    assert_eq!(&bytes[piece + 40..piece + 72], [0xA2; 32]);

    // Every piece, with its component and index.
    let pieces: Vec<_> = manifest.pieces().collect();
    assert_eq!(
        pieces,
        [
            (ComponentId::Utxos, 0, utxos.pieces[0]),
            (ComponentId::Utxos, 1, utxos.pieces[1]),
            (
                ComponentId::ValuePools,
                0,
                manifest.components[23].pieces[0]
            ),
        ],
    );
    assert_eq!(
        manifest.total_piece_bytes(),
        MAX_PIECE_BYTES as u64 + 1 + 57
    );

    // A manifest on a network with a parameters hash carries it after the index mode.
    let regtest = Network::new_regtest(Default::default());
    let bytes = empty_manifest(&regtest).encode().expect("valid");
    assert_eq!(bytes.len(), HEADER_LEN + 32 + 24 * EMPTY_COMPONENT_LEN);
    assert_eq!(&bytes[97..129], [0x55; 32]);
    assert_eq!(&bytes[129..131], 24_u16.to_le_bytes());
}

#[test]
fn manifest_decode_rejects_every_header_violation() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let valid = empty_manifest(&network).encode().expect("valid");
    let decode = |bytes: &[u8]| Manifest::decode(bytes, &network);

    let mut bad_magic = valid.clone();
    bad_magic[0] ^= 0x01;
    assert_err!(decode(&bad_magic), FormatError::BadMagic);

    let mut unsupported = valid.clone();
    unsupported[20..22].copy_from_slice(&1_u16.to_le_bytes());
    assert_err!(decode(&unsupported), FormatError::UnsupportedFormat(1));

    let mut index_mode = valid.clone();
    index_mode[96] = 1;
    assert_err!(decode(&index_mode), FormatError::UnknownIndexMode(1));

    let mut too_high = valid.clone();
    too_high[54..58].copy_from_slice(&(block::Height::MAX.0 + 1).to_le_bytes());
    assert_err!(
        decode(&too_high),
        FormatError::InvalidHeight(height) if height == block::Height::MAX.0 + 1
    );

    let mut window_above = valid.clone();
    window_above[90..94].copy_from_slice(&(HEIGHT + 1).to_le_bytes());
    assert_err!(
        decode(&window_above),
        FormatError::WindowAboveHeight { window_start, height }
            if window_start == HEIGHT + 1 && height == HEIGHT
    );

    let mut count = valid.clone();
    count[97..99].copy_from_slice(&23_u16.to_le_bytes());
    assert_err!(decode(&count), FormatError::WrongComponentCount(23));

    let mut trailing = valid.clone();
    trailing.push(0);
    assert_err!(
        decode(&trailing),
        FormatError::TrailingBytes {
            what: "manifest",
            count: 1
        }
    );

    // Every strict prefix is truncated: the decoder never reads past the bytes it has.
    for cut in 0..valid.len() {
        assert_err!(decode(&valid[..cut]), FormatError::Truncated { .. });
    }

    // Over the manifest limit, before any field is looked at.
    let oversized = vec![0; MAX_MANIFEST_BYTES + 1];
    assert_err!(
        decode(&oversized),
        FormatError::ManifestTooLarge { len } if len == MAX_MANIFEST_BYTES + 1
    );
}

#[test]
fn manifest_decode_rejects_every_component_violation() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let valid = empty_manifest(&network).encode().expect("valid");
    let decode = |bytes: &[u8]| Manifest::decode(bytes, &network);
    let first = component_offset(ComponentId::HashByHeight);
    let second = component_offset(ComponentId::HeightByHash);

    let mut unknown = valid.clone();
    unknown[first..first + 2].copy_from_slice(&24_u16.to_le_bytes());
    assert_err!(decode(&unknown), FormatError::UnknownComponent(24));

    let mut swapped = valid.clone();
    swapped[first..first + 2].copy_from_slice(&1_u16.to_le_bytes());
    swapped[second..second + 2].copy_from_slice(&0_u16.to_le_bytes());
    assert_err!(
        decode(&swapped),
        FormatError::ComponentOutOfOrder {
            expected: ComponentId::HashByHeight,
            found: ComponentId::HeightByHash,
        }
    );

    // A component listed twice displaces the one that should follow it.
    let mut duplicate = valid.clone();
    duplicate[second..second + 2].copy_from_slice(&0_u16.to_le_bytes());
    assert_err!(
        decode(&duplicate),
        FormatError::ComponentOutOfOrder {
            expected: ComponentId::HeightByHash,
            found: ComponentId::HashByHeight,
        }
    );

    // Entries without pieces, in the last component so the layout is unchanged.
    let last = component_offset(ComponentId::ValuePools);
    let mut entries_without_pieces = valid.clone();
    entries_without_pieces[last + 2..last + 10].copy_from_slice(&1_u64.to_le_bytes());
    assert_err!(
        decode(&entries_without_pieces),
        FormatError::InconsistentCounts {
            component: ComponentId::ValuePools,
            entry_count: 1,
            piece_count: 0,
        }
    );

    // A piece count that claims more entries than there are bytes is rejected before anything is
    // allocated for it.
    let mut claimed = valid.clone();
    claimed[last + 10..last + 14].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_err!(
        decode(&claimed),
        FormatError::Truncated { what: "piece list" }
    );
    let mut one_claimed = valid.clone();
    one_claimed[last + 10..last + 14].copy_from_slice(&1_u32.to_le_bytes());
    assert_err!(
        decode(&one_claimed),
        FormatError::Truncated { what: "piece list" }
    );

    // The piece size bounds, and pieces without entries.
    let mut with_piece = empty_manifest(&network);
    with_piece.components[23].pieces.push(PieceManifest {
        size: 1,
        hash: ObjectHash([0xC1; 32]),
    });
    assert_err!(
        with_piece.encode(),
        FormatError::InconsistentCounts {
            component: ComponentId::ValuePools,
            entry_count: 0,
            piece_count: 1,
        }
    );
    with_piece.components[23].entry_count = 1;
    let bytes = with_piece.encode().expect("valid");
    let piece = last + 14;
    assert_eq!(bytes.len(), valid.len() + PIECE_LEN);

    let mut empty_piece = bytes.clone();
    empty_piece[piece..piece + 4].copy_from_slice(&0_u32.to_le_bytes());
    assert_err!(
        decode(&empty_piece),
        FormatError::EmptyPiece {
            component: ComponentId::ValuePools
        }
    );

    let mut large_piece = bytes.clone();
    large_piece[piece..piece + 4].copy_from_slice(&(MAX_PIECE_BYTES as u32 + 1).to_le_bytes());
    assert_err!(
        decode(&large_piece),
        FormatError::PieceTooLarge {
            component: ComponentId::ValuePools,
            size,
        } if size == MAX_PIECE_BYTES as u32 + 1
    );
    with_piece.components[23].pieces[0].size = 0;
    assert_err!(with_piece.encode(), FormatError::EmptyPiece { .. });
    with_piece.components[23].pieces[0].size = MAX_PIECE_BYTES as u32 + 1;
    assert_err!(with_piece.encode(), FormatError::PieceTooLarge { .. });
}

#[test]
fn manifest_encode_checks_the_structure_and_the_limit() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let mut missing = empty_manifest(&network);
    missing.components.pop();
    assert_err!(missing.encode(), FormatError::WrongComponentCount(23));

    let mut unordered = empty_manifest(&network);
    unordered.components.swap(0, 1);
    assert_err!(
        unordered.encode(),
        FormatError::ComponentOutOfOrder {
            expected: ComponentId::HashByHeight,
            found: ComponentId::HeightByHash,
        }
    );

    let mut index_mode = empty_manifest(&network);
    index_mode.index_mode = 7;
    assert_err!(index_mode.encode(), FormatError::UnknownIndexMode(7));

    let mut window_above = empty_manifest(&network);
    window_above.window_start = block::Height(HEIGHT + 1);
    assert_err!(window_above.encode(), FormatError::WindowAboveHeight { .. });

    // Enough pieces to take the manifest over 1 MiB.
    let mut oversized = empty_manifest(&network);
    let pieces = MAX_MANIFEST_BYTES / PIECE_LEN + 1;
    oversized.components[6].entry_count = 1;
    oversized.components[6].pieces = vec![
        PieceManifest {
            size: 1,
            hash: ObjectHash([0; 32]),
        };
        pieces
    ];
    assert_err!(oversized.encode(), FormatError::ManifestTooLarge { .. });
}

/// Encodes `records` as one piece with [`PieceWriter`].
fn encode_piece(records: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut writer = PieceWriter::new(Vec::new());
    for (key, value) in records {
        writer.push(key, value).expect("valid record");
    }
    assert_eq!(writer.records(), records.len() as u64);
    writer.finish().expect("flushes")
}

/// Decodes every record of `bytes` with [`PieceReader`].
fn decode_piece(bytes: &[u8]) -> Result<Vec<PieceRecord>, FormatError> {
    let mut reader = PieceReader::new(Cursor::new(bytes));
    let mut records = Vec::new();
    while let Some(record) = reader.next_record()? {
        records.push(record);
    }
    assert_eq!(reader.bytes_read(), bytes.len());
    assert_eq!(reader.records(), records.len() as u64);
    Ok(records)
}

#[test]
fn piece_records_round_trip_with_pinned_layout() {
    let _init_guard = zebra_test::init();

    // Empty keys and values are rows too: the chain singletons have a `()` key, and the nullifier
    // sets a `()` value.
    let records: [(&[u8], &[u8]); 4] = [
        (b"", b"singleton"),
        (b"\x00", b""),
        (b"\x00\x01", b"value"),
        (&[0xFF; 300], &[0xAB; 0x1_0000]),
    ];
    let bytes = encode_piece(&records);

    let mut expected = Vec::new();
    expected.extend_from_slice(b"\x00\x09singleton");
    expected.extend_from_slice(b"\x01\x00\x00");
    expected.extend_from_slice(b"\x02\x00\x01\x05value");
    expected.extend_from_slice(&[0xFD, 0x2C, 0x01]);
    expected.extend_from_slice(&[0xFF; 300]);
    expected.extend_from_slice(&[0xFE, 0x00, 0x00, 0x01, 0x00]);
    expected.extend_from_slice(&[0xAB; 0x1_0000]);
    assert_eq!(bytes, expected);

    let decoded = decode_piece(&bytes).expect("valid piece");
    let expected: Vec<PieceRecord> = records
        .iter()
        .map(|(key, value)| PieceRecord {
            key: key.to_vec(),
            value: value.to_vec(),
        })
        .collect();
    assert_eq!(decoded, expected);
    for record in &expected {
        assert_eq!(
            record.encode().expect("valid").len(),
            PieceRecord::encoded_len(record.key.len(), record.value.len()).expect("in bounds"),
        );
    }

    // An empty piece has no records.
    assert_eq!(decode_piece(&[]).expect("valid"), Vec::<PieceRecord>::new());
    let empty = PieceWriter::new(Vec::new());
    assert!(empty.is_empty());
    assert_eq!(empty.bytes_written(), 0);
}

#[test]
fn piece_reader_rejects_every_record_violation() {
    let _init_guard = zebra_test::init();

    // A repeated key, then a lower key.
    assert_err!(
        decode_piece(b"\x01\x02\x00\x01\x02\x00"),
        FormatError::KeyNotIncreasing
    );
    assert_err!(
        decode_piece(b"\x01\x02\x00\x01\x01\x00"),
        FormatError::KeyNotIncreasing
    );

    // A non-canonical key length: 5 encoded in three bytes.
    assert_err!(
        decode_piece(b"\xFD\x05\x00abcde\x00"),
        FormatError::InvalidCompactSize {
            what: "record key length",
            ..
        }
    );
    assert_err!(
        decode_piece(b"\x01k\xFD\x01\x00v"),
        FormatError::InvalidCompactSize {
            what: "record value length",
            ..
        }
    );

    // The stream ends inside a record: after the key length, inside the key, before the value
    // length, inside the value length, and inside the value.
    let record = encode_piece(&[(&[0xAA; 300], b"value")]);
    for cut in 1..record.len() {
        assert_err!(decode_piece(&record[..cut]), FormatError::Truncated { .. });
    }

    // A length over the piece limit is rejected before any of it is read.
    let mut huge_key = Vec::new();
    huge_key.extend_from_slice(&[0xFE]);
    huge_key.extend_from_slice(&(MAX_PIECE_BYTES as u32 + 1).to_le_bytes());
    assert_err!(
        decode_piece(&huge_key),
        FormatError::RecordTooLarge { len } if len == MAX_PIECE_BYTES as u64 + 1
    );
    let mut huge_value = b"\x01k".to_vec();
    huge_value.extend_from_slice(&[0xFF]);
    huge_value.extend_from_slice(&(u64::MAX).to_le_bytes());
    assert_err!(
        decode_piece(&huge_value),
        FormatError::RecordTooLarge { len: u64::MAX }
    );

    // Two records that fit the limit each, but not together.
    let half = vec![0xCD; MAX_PIECE_BYTES / 2];
    let mut over = Vec::new();
    PieceRecord::write(b"a", &half, &mut over).expect("fits on its own");
    PieceRecord::write(b"b", &half, &mut over).expect("fits on its own");
    assert_err!(decode_piece(&over), FormatError::PieceBytesExceeded);
}

#[test]
fn piece_writer_enforces_order_and_the_limit() {
    let _init_guard = zebra_test::init();

    let mut writer = PieceWriter::new(Vec::new());
    writer.push(b"b", b"").expect("valid");
    assert_err!(writer.push(b"b", b""), FormatError::KeyNotIncreasing);
    assert_err!(writer.push(b"a", b""), FormatError::KeyNotIncreasing);
    writer.push(b"c", b"").expect("valid");
    assert_eq!(writer.records(), 2);
    assert_eq!(writer.bytes_written(), 6);

    // A record that fits in no piece is an error, not a full piece.
    let too_large = vec![0; MAX_PIECE_BYTES];
    assert_err!(
        writer.fits(b"d", &too_large),
        FormatError::RecordTooLarge { .. }
    );
    assert_err!(
        writer.push(b"d", &too_large),
        FormatError::RecordTooLarge { .. }
    );
    assert_err!(
        PieceRecord::write(b"d", &too_large, Vec::new()),
        FormatError::RecordTooLarge { .. }
    );

    // The largest record: exactly at the limit with its length prefixes.
    let largest = vec![0; MAX_PIECE_BYTES - 1 - 1 - 5];
    assert_eq!(
        PieceRecord::encoded_len(1, largest.len()),
        Some(MAX_PIECE_BYTES)
    );
    let mut full = PieceWriter::new(Vec::new());
    assert!(full.fits(b"a", &largest).expect("in bounds"));
    full.push(b"a", &largest).expect("fills the piece");
    assert_eq!(full.bytes_written(), MAX_PIECE_BYTES);

    // The piece is full: the next record goes in the next piece, and this one is unchanged.
    assert!(!full.fits(b"b", b"").expect("in bounds"));
    assert_err!(full.push(b"b", b""), FormatError::PieceFull);
    assert_eq!(full.records(), 1);
    assert_eq!(full.bytes_written(), MAX_PIECE_BYTES);
    let bytes = full.finish().expect("flushes");
    assert_eq!(bytes.len(), MAX_PIECE_BYTES);
    assert_eq!(decode_piece(&bytes).expect("valid").len(), 1);
}
