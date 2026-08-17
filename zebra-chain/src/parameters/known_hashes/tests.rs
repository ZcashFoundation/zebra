//! Tests for the every-block known-hash list constants and verification.

use sha2::{Digest, Sha256};

use super::*;
use crate::parameters::Network;

/// Builds a single-chunk synthetic spec over `hashes`, returning the spec and
/// the canonical v2 chunk bytes.
///
/// Leaks the spec and its strings: test-only, bounded by the number of tests.
fn synthetic_spec(hashes: &[block::Hash]) -> (&'static KnownHashListSpec, Vec<u8>) {
    let blocks: Vec<([u8; 32], u8)> = hashes.iter().map(|h| (h.0, 1)).collect();
    let bytes = chunk_v2::encode(&blocks, false, &[], &[], false);

    let chunk_hash: &'static str = Box::leak(hex::encode(Sha256::digest(&bytes)).into_boxed_str());

    let spec = Box::leak(Box::new(KnownHashListSpec {
        max_height: block::Height((hashes.len() - 1) as u32),
        chunk_blocks: HASHES_PER_CHUNK,
        chunk_hashes: Box::leak(Box::new([chunk_hash])),
    }));

    (spec, bytes)
}

/// A run of distinct fake hashes starting with the Mainnet genesis hash, so
/// synthetic lists pass the genesis check.
fn fake_hashes(len: usize) -> Vec<block::Hash> {
    let genesis = Network::Mainnet.genesis_hash();

    (0..len)
        .map(|i| {
            if i == 0 {
                genesis
            } else {
                let mut h = [0xAA; 32];
                h[..8].copy_from_slice(&(i as u64).to_le_bytes());
                block::Hash(h)
            }
        })
        .collect()
}

#[test]
fn mainnet_spec_constants_well_formed() {
    let _init_guard = zebra_test::init();

    let spec = &MAINNET_KNOWN_HASHES;

    assert_eq!(spec.chunk_hashes.len(), 23);
    assert!(!spec.is_empty());
    assert_eq!(spec.len(), 3_373_207);

    for hash in spec.chunk_hashes {
        assert_eq!(hash.len(), 64, "SHA-256 hex must be 64 chars: {hash}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 hex must be lowercase hex: {hash}"
        );
    }

    // 22 full chunks of 150,000 plus the remainder.
    assert_eq!(spec.chunk_len(0), 150_000);
    assert_eq!(spec.chunk_len(21), 150_000);
    assert_eq!(spec.chunk_len(22), 3_373_207 - 22 * 150_000);
}

#[test]
fn spec_chunk_counts_match_max_heights() {
    let _init_guard = zebra_test::init();

    for spec in [&MAINNET_KNOWN_HASHES, &TESTNET_KNOWN_HASHES] {
        let expected_chunks = spec.len().div_ceil(u64::from(spec.chunk_blocks));
        assert_eq!(
            spec.chunk_count() as u64,
            expected_chunks,
            "the pinned chunk count must cover exactly the pinned max height",
        );

        let total: u64 = (0..spec.chunk_count()).map(|i| spec.chunk_len(i)).sum();
        assert_eq!(
            total,
            spec.len(),
            "per-chunk lengths must sum to the list length",
        );

        assert_eq!(spec.chunk_index_for(block::Height(0)), Some(0));
        assert_eq!(
            spec.chunk_index_for(spec.max_height),
            Some(spec.chunk_count() - 1),
        );
        assert_eq!(
            spec.chunk_index_for(block::Height(spec.max_height.0 + 1)),
            None,
        );
    }
}

#[test]
fn for_network_coverage() {
    let _init_guard = zebra_test::init();

    assert!(KnownHashListSpec::for_network(&Network::Mainnet).is_some());
    assert!(KnownHashListSpec::for_network(&Network::new_default_testnet()).is_some());
    assert!(KnownHashListSpec::for_network(&Network::new_regtest(Default::default())).is_none());
}

#[test]
fn synthetic_chunk_verifies_and_parses() {
    let _init_guard = zebra_test::init();

    let hashes = fake_hashes(5);
    let (spec, bytes) = synthetic_spec(&hashes);

    let parsed = spec
        .verify_chunk_bytes(&Network::Mainnet, 0, &bytes)
        .expect("a canonical chunk matching its pin verifies");

    assert_eq!(parsed.block_count(), 5);
    for (i, hash) in hashes.iter().enumerate() {
        assert_eq!(parsed.block_hash(i as u32), hash.0);
    }
}

#[test]
fn corrupt_chunk_rejected() {
    let _init_guard = zebra_test::init();

    let (spec, mut bytes) = synthetic_spec(&fake_hashes(5));

    // Flip one bit in a block hash: the structure stays valid, the pin fails.
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;

    assert!(matches!(
        spec.verify_chunk_bytes(&Network::Mainnet, 0, &bytes),
        Err(KnownHashError::ChunkHashMismatch { index: 0, .. }),
    ));
}

#[test]
fn truncated_chunk_rejected() {
    let _init_guard = zebra_test::init();

    let (spec, bytes) = synthetic_spec(&fake_hashes(5));

    assert!(matches!(
        spec.verify_chunk_bytes(&Network::Mainnet, 0, &bytes[..bytes.len() - 1]),
        Err(KnownHashError::ChunkV2 { index: 0, .. }),
    ));
}

#[test]
fn wrong_block_count_rejected() {
    let _init_guard = zebra_test::init();

    let (spec, _bytes) = synthetic_spec(&fake_hashes(5));

    // A structurally valid chunk whose span disagrees with the spec.
    let short_blocks: Vec<([u8; 32], u8)> = fake_hashes(4).iter().map(|h| (h.0, 1)).collect();
    let short_bytes = chunk_v2::encode(&short_blocks, false, &[], &[], false);

    assert!(matches!(
        spec.verify_chunk_bytes(&Network::Mainnet, 0, &short_bytes),
        Err(KnownHashError::ChunkLength {
            index: 0,
            expected_blocks: 5,
            actual_blocks: 4,
            ..
        }),
    ));
}

#[test]
fn genesis_mismatch_rejected() {
    let _init_guard = zebra_test::init();

    // A list starting with a non-genesis hash: the pin matches, the network
    // check fails.
    let mut hashes = fake_hashes(5);
    hashes[0] = block::Hash([0xBB; 32]);
    let (spec, bytes) = synthetic_spec(&hashes);

    assert!(matches!(
        spec.verify_chunk_bytes(&Network::Mainnet, 0, &bytes),
        Err(KnownHashError::GenesisMismatch { .. }),
    ));
}

#[test]
fn out_of_range_chunk_index_rejected() {
    let _init_guard = zebra_test::init();

    let (spec, bytes) = synthetic_spec(&fake_hashes(5));

    assert!(matches!(
        spec.verify_chunk_bytes(&Network::Mainnet, 1, &bytes),
        Err(KnownHashError::ChunkIndexOutOfRange {
            index: 1,
            chunk_count: 1,
        }),
    ));
}

#[test]
fn non_genesis_chunks_skip_the_genesis_check() {
    let _init_guard = zebra_test::init();

    // A two-chunk spec: chunk 1 starts with an arbitrary hash and must not be
    // held to the genesis check.
    let chunk_blocks = 3u32;
    let first = fake_hashes(3);
    let second = vec![block::Hash([0xCC; 32]), block::Hash([0xCD; 32])];

    let encode = |hashes: &[block::Hash]| {
        let blocks: Vec<([u8; 32], u8)> = hashes.iter().map(|h| (h.0, 1)).collect();
        chunk_v2::encode(&blocks, false, &[], &[], false)
    };
    let first_bytes = encode(&first);
    let second_bytes = encode(&second);

    let leak_hash = |bytes: &[u8]| -> &'static str {
        Box::leak(hex::encode(Sha256::digest(bytes)).into_boxed_str())
    };
    let spec = Box::leak(Box::new(KnownHashListSpec {
        max_height: block::Height(4),
        chunk_blocks,
        chunk_hashes: Box::leak(Box::new([
            leak_hash(&first_bytes),
            leak_hash(&second_bytes),
        ])),
    }));

    spec.verify_chunk_bytes(&Network::Mainnet, 0, &first_bytes)
        .expect("chunk 0 verifies with the genesis check");
    spec.verify_chunk_bytes(&Network::Mainnet, 1, &second_bytes)
        .expect("chunk 1 verifies without the genesis check");
}
