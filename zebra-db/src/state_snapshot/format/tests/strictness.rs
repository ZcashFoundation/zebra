//! Strictness and determinism of the snapshot format: every manifest and
//! piece has exactly one encoding, so equal snapshots have equal hashes, and
//! the piece reader is independent of how its stream delivers the bytes.

use std::io::{self, Cursor, Read};

use zebra_chain::{block, parameters::Network, state_snapshot::ObjectHash};

use crate::state_snapshot::format::{
    ComponentId, ComponentManifest, Manifest, PieceManifest, PieceReader, PieceRecord, PieceWriter,
    INDEX_MODE_DEFAULT, MAX_PIECE_BYTES,
};

/// A manifest for `network` with pieces in several components, and its
/// encoding.
fn manifest(network: &Network) -> (Manifest, Vec<u8>) {
    let height = block::Height(3_522_060);
    let manifest = Manifest {
        genesis_hash: network.genesis_hash(),
        height,
        block_hash: block::Hash([0x11; 32]),
        window_start: block::Height(height.0 - 2303),
        db_format_major: 30,
        index_mode: INDEX_MODE_DEFAULT,
        parameters_hash: network.state_snapshot_parameters_hash(),
        components: ComponentId::ALL
            .into_iter()
            .map(|id| {
                let pieces: Vec<PieceManifest> = (0..id.to_u16() % 3)
                    .map(|index| PieceManifest {
                        size: 1000 * u32::from(id.to_u16()) + u32::from(index) + 1,
                        hash: ObjectHash([u8::try_from(id.to_u16()).expect("28 ids"); 32]),
                    })
                    .collect();
                ComponentManifest {
                    id,
                    entry_count: u64::try_from(pieces.len()).expect("a few pieces") * 7,
                    pieces,
                }
            })
            .collect(),
    };
    let bytes = manifest.encode().expect("a valid manifest");
    (manifest, bytes)
}

/// Records with every CompactSize width the piece limit allows, and empty
/// keys and values.
fn records() -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        (Vec::new(), b"singleton".to_vec()),
        (vec![0x00], Vec::new()),
        (vec![0x00, 0x01], vec![0x5A; 0xFC]),
        (vec![0x01], vec![0x5B; 0xFD]),
        (vec![0x02; 0xFD], vec![0x5C; 3]),
        (vec![0x03], vec![0x5D; 0x1_0000]),
    ]
}

/// Encodes `records` as one piece.
fn piece(records: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut writer = PieceWriter::new(Vec::new());
    for (key, value) in records {
        writer.push(key, value).expect("valid records");
    }
    writer.finish().expect("flushes")
}

/// Decodes every record of the piece in `reader`.
fn read_piece(reader: impl Read) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
    let mut reader = PieceReader::new(reader);
    let mut records = Vec::new();
    while let Some(PieceRecord { key, value }) =
        reader.next_record().map_err(|error| error.to_string())?
    {
        records.push((key, value));
    }
    Ok(records)
}

/// Every manifest that decodes has exactly the encoding it was decoded from:
/// flipping any bit pattern in any byte either is refused, or describes a
/// different manifest whose encoding is the changed bytes. So a manifest's hash
/// is a function of its contents.
#[test]
fn manifests_have_exactly_one_encoding() {
    let _init_guard = zebra_test::init();

    for network in [Network::Mainnet, Network::new_regtest(Default::default())] {
        let (manifest, bytes) = manifest(&network);
        assert_eq!(
            Manifest::decode(&bytes, &network).expect("round trips"),
            manifest
        );

        let mut accepted = 0;
        for position in 0..bytes.len() {
            for mask in [0x01, 0x80, 0xFF] {
                let mut changed = bytes.clone();
                changed[position] ^= mask;
                if let Ok(decoded) = Manifest::decode(&changed, &network) {
                    assert_ne!(decoded, manifest, "byte {position} changed nothing");
                    assert_eq!(
                        decoded.encode().expect("a decoded manifest is valid"),
                        changed,
                        "byte {position} has two encodings",
                    );
                    accepted += 1;
                }
            }
        }
        // Hashes and heights take any value, so many changes still decode.
        assert!(accepted > 0, "{network}");

        // Equal manifests encode equally.
        assert_eq!(
            manifest.clone().encode().expect("valid"),
            manifest.encode().expect("valid")
        );
    }
}

/// Every piece that decodes has exactly the encoding it was decoded from, as
/// for manifests: lengths are canonical CompactSizes, and keys are ordered.
#[test]
fn pieces_have_exactly_one_encoding() {
    let _init_guard = zebra_test::init();

    let records = records();
    let bytes = piece(&records);
    assert_eq!(read_piece(Cursor::new(&bytes)), Ok(records.clone()));

    // The length prefixes are where alternative encodings could hide, so every byte of the first
    // 600 is changed, which covers the 1-, 3- and 5-byte prefixes; the rest of the piece is
    // payload.
    let mut accepted = 0;
    for position in 0..bytes.len().min(600) {
        for mask in [0x01, 0x80, 0xFF] {
            let mut changed = bytes.clone();
            changed[position] ^= mask;
            if let Ok(decoded) = read_piece(Cursor::new(&changed)) {
                let mut encoded = Vec::new();
                for (key, value) in &decoded {
                    PieceRecord::write(key, value, &mut encoded).expect("decoded records fit");
                }
                assert_eq!(encoded, changed, "byte {position} has two encodings");
                accepted += 1;
            }
        }
    }
    assert!(accepted > 0);

    // The same records always make the same piece.
    assert_eq!(piece(&records), bytes);
}

/// Splitting a piece at a record boundary splits its records; a split
/// anywhere else is refused.
#[test]
fn pieces_end_only_at_record_boundaries() {
    let _init_guard = zebra_test::init();

    let records = records();
    let mut boundaries = vec![0];
    let mut bytes = Vec::new();
    for (key, value) in &records {
        PieceRecord::write(key, value, &mut bytes).expect("fits");
        boundaries.push(bytes.len());
    }

    for (count, boundary) in boundaries.iter().enumerate() {
        assert_eq!(
            read_piece(Cursor::new(&bytes[..*boundary])),
            Ok(records[..count].to_vec()),
            "{count} records",
        );
    }
    for cut in 0..bytes.len() {
        if !boundaries.contains(&cut) {
            assert!(
                read_piece(Cursor::new(&bytes[..cut])).is_err(),
                "cut at {cut}"
            );
        }
    }
}

/// A reader that delivers one byte per call, and is interrupted before each.
struct Trickle<R> {
    inner: R,
    interrupt: bool,
}

impl<R: Read> Read for Trickle<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let len = buf.len().min(1);
        self.inner.read(&mut buf[..len])
    }
}

/// The reader decodes the same records however its stream delivers them,
/// including short reads and interrupted reads, which a reader retries.
#[test]
fn piece_reader_handles_short_and_interrupted_reads() {
    let _init_guard = zebra_test::init();

    let records = records();
    let bytes = piece(&records);
    let trickle = Trickle {
        inner: Cursor::new(&bytes),
        interrupt: false,
    };
    assert_eq!(read_piece(trickle), Ok(records));

    // Errors other than interruptions are reported.
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    assert!(read_piece(Failing).is_err());
}

/// The largest piece holds exactly `MAX_PIECE_BYTES`: the writer fills it, and
/// the reader accepts it, but neither goes one byte further.
#[test]
fn the_piece_limit_is_exact_for_writer_and_reader() {
    let _init_guard = zebra_test::init();

    // Two records that fill a piece exactly.
    let first = vec![0x01; 1 << 20];
    let first_len = PieceRecord::encoded_len(1, first.len()).expect("fits");
    let second_value_len = MAX_PIECE_BYTES - first_len - 1 - 1 - 5;
    let second = vec![0x02; second_value_len];
    let mut writer = PieceWriter::new(Vec::new());
    writer.push(b"a", &first).expect("fits");
    writer.push(b"b", &second).expect("fills the piece");
    assert_eq!(writer.bytes_written(), MAX_PIECE_BYTES);
    let full = writer.finish().expect("flushes");
    assert_eq!(read_piece(Cursor::new(&full)).expect("valid").len(), 2);

    // One more byte, in a value that fits on its own, is refused by the reader.
    let mut over = Vec::new();
    PieceRecord::write(b"a", &first, &mut over).expect("fits");
    PieceRecord::write(b"b", &vec![0x02; second_value_len + 1], &mut over).expect("fits");
    assert_eq!(over.len(), MAX_PIECE_BYTES + 1);
    assert!(read_piece(Cursor::new(&over)).is_err());
}
