//! Tests for the known-hash chunk format v2 encoder and parser.

use super::*;

/// Builds `n` distinct fake block hashes, the byte `i` repeated 32 times for
/// height `i`, so each hash is easy to recognize in assertions.
fn fake_hashes(n: usize) -> Vec<[u8; HASH_BYTES]> {
    (0..n)
        .map(|i| {
            let mut h = [0u8; HASH_BYTES];
            // i fits a u8 only for small n; spread it across the first bytes.
            h[..8].copy_from_slice(&(i as u64).to_le_bytes());
            h
        })
        .collect()
}

/// A fake tree root: the byte `seed` repeated 32 times.
fn fake_root(seed: u8) -> [u8; HASH_BYTES] {
    [seed; HASH_BYTES]
}

/// Builds `(hash, hint)` pairs from `hashes`, with hint `(i % 255) + 1`.
fn blocks_with_hints(hashes: &[[u8; HASH_BYTES]]) -> Vec<([u8; HASH_BYTES], u8)> {
    hashes
        .iter()
        .enumerate()
        // hint is always in 1..=255, never the absent/default sentinel.
        .map(|(i, h)| (*h, ((i % 255) as u8) + 1))
        .collect()
}

/// Hand-builds the full 16-byte v2 header for `flags` and `block_count`,
/// including the trailing reserved padding, for the malformed-body tests that
/// construct chunk bytes by hand.
fn v2_header(flags: u8, block_count: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(VERSION);
    bytes.push(flags);
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&block_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

#[test]
fn header_layout_is_stable() {
    let hashes = fake_hashes(3);
    let blocks = blocks_with_hints(&hashes);
    let bytes = encode(&blocks, true, &[], &[], true);

    assert_eq!(&bytes[0..4], b"ZKH2");
    assert_eq!(bytes[4], VERSION);
    // has_hints | has_tree_roots
    assert_eq!(bytes[5], 0b0000_0011);
    assert_eq!(&bytes[6..8], &[0, 0]);
    assert_eq!(&bytes[8..12], &3u32.to_le_bytes());
    assert_eq!(HEADER_LEN, 16);
}

#[test]
fn round_trip_with_hints_and_sparse_roots() {
    let hashes = fake_hashes(10);
    let blocks = blocks_with_hints(&hashes);

    let sapling = vec![
        TreeRoot {
            rel_height: 0,
            root: fake_root(0x11),
        },
        TreeRoot {
            rel_height: 4,
            root: fake_root(0x22),
        },
        TreeRoot {
            rel_height: 9,
            root: fake_root(0x33),
        },
    ];
    let orchard = vec![TreeRoot {
        rel_height: 7,
        root: fake_root(0x44),
    }];

    let bytes = encode(&blocks, true, &sapling, &orchard, true);
    let parsed = ParsedChunk::parse(&bytes).expect("valid chunk parses");

    assert_eq!(parsed.block_count(), 10);
    assert!(parsed.has_hints());

    for (i, (hash, hint)) in blocks.iter().enumerate() {
        let rel = i as u32;
        assert_eq!(parsed.block_hash(rel), *hash, "hash mismatch at rel {rel}");
        assert_eq!(parsed.size_hint(rel), *hint, "hint mismatch at rel {rel}");
    }

    assert_eq!(parsed.sapling_roots(), sapling);
    assert_eq!(parsed.orchard_roots(), orchard);
}

#[test]
fn determinism_same_inputs_same_bytes() {
    let hashes = fake_hashes(50);
    let blocks = blocks_with_hints(&hashes);
    let sapling = vec![
        TreeRoot {
            rel_height: 1,
            root: fake_root(1),
        },
        TreeRoot {
            rel_height: 49,
            root: fake_root(2),
        },
    ];
    let orchard = vec![TreeRoot {
        rel_height: 25,
        root: fake_root(3),
    }];

    let a = encode(&blocks, true, &sapling, &orchard, true);
    let b = encode(&blocks, true, &sapling, &orchard, true);
    assert_eq!(a, b, "the same inputs must encode to byte-identical chunks");
}

#[test]
fn at_or_before_boundaries() {
    let hashes = fake_hashes(20);
    let blocks = blocks_with_hints(&hashes);

    // Records at rel 3, 7, 15.
    let sapling = vec![
        TreeRoot {
            rel_height: 3,
            root: fake_root(0xA0),
        },
        TreeRoot {
            rel_height: 7,
            root: fake_root(0xB0),
        },
        TreeRoot {
            rel_height: 15,
            root: fake_root(0xC0),
        },
    ];
    let bytes = encode(&blocks, true, &sapling, &[], true);
    let parsed = ParsedChunk::parse(&bytes).expect("valid chunk parses");

    // Below the first record: None.
    assert_eq!(parsed.sapling_root_at_or_before(0), None);
    assert_eq!(parsed.sapling_root_at_or_before(2), None);
    // Exactly on a record.
    assert_eq!(parsed.sapling_root_at_or_before(3), Some(fake_root(0xA0)));
    assert_eq!(parsed.sapling_root_at_or_before(7), Some(fake_root(0xB0)));
    assert_eq!(parsed.sapling_root_at_or_before(15), Some(fake_root(0xC0)));
    // Between records: the largest <= rel.
    assert_eq!(parsed.sapling_root_at_or_before(4), Some(fake_root(0xA0)));
    assert_eq!(parsed.sapling_root_at_or_before(6), Some(fake_root(0xA0)));
    assert_eq!(parsed.sapling_root_at_or_before(14), Some(fake_root(0xB0)));
    // At/after the last record (still within the span).
    assert_eq!(parsed.sapling_root_at_or_before(16), Some(fake_root(0xC0)));
    assert_eq!(parsed.sapling_root_at_or_before(19), Some(fake_root(0xC0)));

    // The orchard section was empty.
    assert_eq!(parsed.orchard_root_at_or_before(19), None);
    assert!(parsed.orchard_roots().is_empty());
}

#[test]
fn hash_only_chunk_uses_default_hint() {
    let hashes = fake_hashes(5);
    let blocks = blocks_with_hints(&hashes);

    // with_hints = false: the per-pair hints are dropped.
    let bytes = encode(&blocks, false, &[], &[], false);
    let parsed = ParsedChunk::parse(&bytes).expect("hash-only chunk parses");

    assert!(!parsed.has_hints());
    for i in 0..5u32 {
        assert_eq!(parsed.block_hash(i), hashes[i as usize]);
        assert_eq!(
            parsed.size_hint(i),
            DEFAULT_SIZE_HINT,
            "hash-only chunk yields the default hint at rel {i}",
        );
    }
    // No tree-root sections, no roots.
    assert_eq!(parsed.sapling_root_at_or_before(4), None);
    assert_eq!(parsed.orchard_root_at_or_before(4), None);
}

#[test]
fn hints_without_tree_roots() {
    let hashes = fake_hashes(4);
    let blocks = blocks_with_hints(&hashes);

    let bytes = encode(&blocks, true, &[], &[], false);
    let parsed = ParsedChunk::parse(&bytes).expect("hinted, treeless chunk parses");

    assert!(parsed.has_hints());
    for (i, (_, hint)) in blocks.iter().enumerate() {
        assert_eq!(parsed.size_hint(i as u32), *hint);
    }
    assert_eq!(parsed.sapling_root_at_or_before(3), None);
    assert_eq!(parsed.orchard_root_at_or_before(3), None);
}

#[test]
fn empty_span_round_trips() {
    let bytes = encode(&[], true, &[], &[], true);
    let parsed = ParsedChunk::parse(&bytes).expect("empty chunk parses");

    assert_eq!(parsed.block_count(), 0);
    assert!(parsed.has_hints());
    assert_eq!(parsed.sapling_root_at_or_before(0), None);
    assert_eq!(parsed.orchard_root_at_or_before(0), None);
    assert!(parsed.sapling_roots().is_empty());
}

#[test]
fn empty_tree_sections_present_but_zero() {
    let hashes = fake_hashes(3);
    let blocks = blocks_with_hints(&hashes);

    // has_tree_roots = true but both lists empty: the two u32 counts are written.
    let bytes = encode(&blocks, true, &[], &[], true);
    // header + 3*32 hashes + 3 hints + 2 zero u32 counts.
    assert_eq!(bytes.len(), HEADER_LEN + 3 * HASH_BYTES + 3 + 8);

    let parsed = ParsedChunk::parse(&bytes).expect("valid chunk parses");
    assert_eq!(parsed.sapling_root_at_or_before(2), None);
    assert_eq!(parsed.orchard_root_at_or_before(2), None);
}

#[test]
fn is_v2_detects_magic() {
    let bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    assert!(is_v2(&bytes));

    // A bare v1 chunk (just hashes) is not v2.
    let v1: Vec<u8> = fake_hashes(2).into_iter().flatten().collect();
    assert!(!is_v2(&v1));
    assert!(!is_v2(&[]));
    assert!(!is_v2(b"ZKH"));
}

#[test]
fn rejects_short_header() {
    assert_eq!(
        ParsedChunk::parse(b"ZKH2\x02"),
        Err(ChunkV2Error::ShortHeader { len: 5 }),
    );
}

#[test]
fn rejects_bad_magic() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    bytes[0] = b'X';
    assert!(matches!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BadMagic { .. })
    ));
}

#[test]
fn rejects_bad_version() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    bytes[4] = 99;
    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BadVersion { actual: 99 }),
    );
}

#[test]
fn rejects_unknown_flags() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    // Set a flag bit outside has_hints | has_tree_roots.
    bytes[5] |= 0b1000_0000;
    assert!(matches!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::UnknownFlags { .. })
    ));
}

#[test]
fn rejects_nonzero_reserved() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    bytes[6] = 1;
    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::NonZeroReserved { actual: 1 }),
    );
}

#[test]
fn rejects_nonzero_reserved_tail() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(1)), true, &[], &[], true);
    // Dirty a byte in the trailing reserved padding (offsets 12..16).
    bytes[12] = 0xAB;
    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::NonZeroReserved { actual: 0x00AB }),
    );
}

#[test]
fn rejects_block_count_too_large() {
    // Hand-build a header declaring more blocks than HASHES_PER_CHUNK.
    let bytes = v2_header(0, HASHES_PER_CHUNK + 1);

    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BlockCountTooLarge {
            block_count: HASHES_PER_CHUNK + 1,
        }),
    );
}

#[test]
fn rejects_truncated_body() {
    let bytes = encode(&blocks_with_hints(&fake_hashes(10)), true, &[], &[], true);
    // Drop the last byte: the declared sections no longer fit.
    let truncated = &bytes[..bytes.len() - 1];
    assert!(matches!(
        ParsedChunk::parse(truncated),
        Err(ChunkV2Error::BodyLength { .. })
    ));
}

#[test]
fn rejects_trailing_bytes() {
    let mut bytes = encode(&blocks_with_hints(&fake_hashes(10)), true, &[], &[], true);
    bytes.push(0xFF);
    assert!(matches!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BodyLength { .. })
    ));
}

#[test]
fn rejects_out_of_range_tree_record() {
    // Hand-build a valid header + hashes + an out-of-range sapling record.
    let n = 4u32;
    let mut bytes = v2_header(0b0000_0010, n); // has_tree_roots only
    for h in fake_hashes(n as usize) {
        bytes.extend_from_slice(&h);
    }
    // sapling: 1 record at rel_height = n (out of range).
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&n.to_le_bytes());
    bytes.extend_from_slice(&fake_root(7));
    // orchard: empty.
    bytes.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BadTreeRecord {
            index: 0,
            rel_height: n,
            block_count: n,
        }),
    );
}

#[test]
fn rejects_non_ascending_tree_records() {
    let n = 10u32;
    let mut bytes = v2_header(0b0000_0010, n); // has_tree_roots only
    for h in fake_hashes(n as usize) {
        bytes.extend_from_slice(&h);
    }
    // sapling: two records, descending (5 then 3).
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&5u32.to_le_bytes());
    bytes.extend_from_slice(&fake_root(1));
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&fake_root(2));
    // orchard: empty.
    bytes.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BadTreeRecord {
            index: 1,
            rel_height: 3,
            block_count: n,
        }),
    );
}

#[test]
fn rejects_duplicate_tree_records() {
    let n = 10u32;
    let mut bytes = v2_header(0b0000_0010, n);
    for h in fake_hashes(n as usize) {
        bytes.extend_from_slice(&h);
    }
    // sapling: two records at the same rel_height (not strictly ascending).
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&fake_root(1));
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&fake_root(2));
    bytes.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(
        ParsedChunk::parse(&bytes),
        Err(ChunkV2Error::BadTreeRecord {
            index: 1,
            rel_height: 4,
            block_count: n,
        }),
    );
}

#[test]
fn single_record_at_or_before() {
    let hashes = fake_hashes(5);
    let blocks = blocks_with_hints(&hashes);
    let sapling = vec![TreeRoot {
        rel_height: 2,
        root: fake_root(0xEE),
    }];
    let bytes = encode(&blocks, true, &sapling, &[], true);
    let parsed = ParsedChunk::parse(&bytes).expect("valid chunk parses");

    assert_eq!(parsed.sapling_root_at_or_before(0), None);
    assert_eq!(parsed.sapling_root_at_or_before(1), None);
    assert_eq!(parsed.sapling_root_at_or_before(2), Some(fake_root(0xEE)));
    assert_eq!(parsed.sapling_root_at_or_before(4), Some(fake_root(0xEE)));
}

#[test]
#[should_panic(expected = "outside the span")]
fn block_hash_out_of_range_panics() {
    let bytes = encode(&blocks_with_hints(&fake_hashes(3)), true, &[], &[], true);
    let parsed = ParsedChunk::parse(&bytes).expect("valid chunk parses");
    let _ = parsed.block_hash(3);
}

#[test]
#[should_panic(expected = "exceeds HASHES_PER_CHUNK")]
fn encode_oversized_span_panics() {
    let blocks = vec![([0u8; HASH_BYTES], 1u8); HASHES_PER_CHUNK as usize + 1];
    let _ = encode(&blocks, true, &[], &[], false);
}

#[test]
#[should_panic(expected = "strictly ascending")]
fn encode_non_ascending_roots_panics() {
    let blocks = blocks_with_hints(&fake_hashes(10));
    let sapling = vec![
        TreeRoot {
            rel_height: 5,
            root: fake_root(1),
        },
        TreeRoot {
            rel_height: 3,
            root: fake_root(2),
        },
    ];
    let _ = encode(&blocks, true, &sapling, &[], true);
}

#[test]
#[should_panic(expected = "outside the span")]
fn encode_out_of_range_root_panics() {
    let blocks = blocks_with_hints(&fake_hashes(4));
    let sapling = vec![TreeRoot {
        rel_height: 4,
        root: fake_root(1),
    }];
    let _ = encode(&blocks, true, &sapling, &[], true);
}

#[test]
fn full_span_round_trips() {
    // A maximal span: HASHES_PER_CHUNK blocks, hash-only (cheap to build).
    let n = HASHES_PER_CHUNK as usize;
    let blocks: Vec<_> = (0..n).map(|_| ([0u8; HASH_BYTES], 0u8)).collect();
    let bytes = encode(&blocks, false, &[], &[], false);
    let parsed = ParsedChunk::parse(&bytes).expect("max-span chunk parses");
    assert_eq!(parsed.block_count(), HASHES_PER_CHUNK);
    assert_eq!(parsed.block_hash(HASHES_PER_CHUNK - 1), [0u8; HASH_BYTES]);
}
