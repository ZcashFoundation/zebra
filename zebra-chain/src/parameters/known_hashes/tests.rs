//! Tests for the every-block known-hash list loader.

use std::path::Path;

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::parameters::Network;

/// The display-order hex of the Mainnet genesis block hash.
const MAINNET_GENESIS_DISPLAY_HEX: &str =
    "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08";

/// The display-order hex of the Mainnet Sapling activation block hash
/// (height 419,200).
const MAINNET_SAPLING_DISPLAY_HEX: &str =
    "00000000025a57200d898ac7f21e26bf29028bbe96ec46e05b2c17cc9db9e4f3";

/// Builds a single-chunk synthetic spec over `hashes`, writing the chunk file
/// into `dir`.
///
/// Leaks the spec and its strings: test-only, bounded by the number of tests.
fn synthetic_spec(dir: &Path, hashes: &[block::Hash]) -> &'static KnownHashListSpec {
    let bytes: Vec<u8> = hashes.iter().flat_map(|h| h.0).collect();

    let chunk_hash: &'static str = Box::leak(hex::encode(Sha256::digest(&bytes)).into_boxed_str());

    let spec = Box::leak(Box::new(KnownHashListSpec {
        max_height: block::Height((hashes.len() - 1) as u32),
        chunk_blocks: 150_000,
        file_prefix: "test-known-hashes",
        size_hint_hash: None,
        chunk_hashes: Box::leak(Box::new([chunk_hash])),
    }));

    std::fs::write(dir.join(spec.chunk_file_name(0)), bytes).expect("test dir is writable");

    spec
}

/// A descending run of distinct fake hashes starting with the Mainnet genesis
/// hash, so synthetic lists pass the genesis check.
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
    assert_eq!(spec.len(), 3_358_432);

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
    assert_eq!(spec.chunk_len(22), 3_358_432 - 22 * 150_000);

    assert_eq!(spec.chunk_file_name(0), "main-known-hashes-00.bin");
    assert_eq!(spec.chunk_file_name(22), "main-known-hashes-22.bin");
}

#[test]
fn for_network_coverage() {
    let _init_guard = zebra_test::init();

    assert!(KnownHashListSpec::for_network(&Network::Mainnet).is_some());
    assert!(KnownHashListSpec::for_network(&Network::new_default_testnet()).is_none());
}

/// Loads the real bundled Mainnet assets: verifies all 23 SHA-256 constants,
/// the genesis cross-check, and known block hashes at era boundaries.
#[test]
fn mainnet_assets_load_and_verify() {
    let _init_guard = zebra_test::init();

    let mut list = KnownHashList::open(&Network::Mainnet, None)
        .expect("bundled mainnet assets load and verify")
        .expect("mainnet has a bundled list");

    assert_eq!(list.max_height(), block::Height(3_358_431));

    // After open(), no chunks are resident (verify-then-drop).
    assert_eq!(list.resident_chunks(), 0);

    let genesis = list
        .hash(block::Height(0))
        .expect("chunk 0 loads")
        .expect("height 0 is covered");
    assert_eq!(genesis, Network::Mainnet.genesis_hash());
    assert_eq!(genesis.to_string(), MAINNET_GENESIS_DISPLAY_HEX);

    let sapling = list
        .hash(block::Height(419_200))
        .expect("chunk 2 loads")
        .expect("height 419,200 is covered");
    assert_eq!(sapling.to_string(), MAINNET_SAPLING_DISPLAY_HEX);

    assert!(list
        .hash(block::Height(3_358_431))
        .expect("last chunk loads")
        .is_some());
    assert!(list
        .hash(block::Height(3_358_432))
        .expect("no load needed")
        .is_none());
    assert!(list
        .hash(block::Height::MAX)
        .expect("no load needed")
        .is_none());
}

/// Sequential lookups across chunk boundaries keep at most two chunks
/// resident, and `release_below` drops chunks behind the frontier.
#[test]
fn windowed_residency() {
    let _init_guard = zebra_test::init();

    let mut list = KnownHashList::open(&Network::Mainnet, None)
        .expect("bundled mainnet assets load and verify")
        .expect("mainnet has a bundled list");

    // Walk lookups across three chunks (0, 1, 2).
    for height in [0, 149_999, 150_000, 290_000, 300_000, 320_000] {
        list.hash(block::Height(height)).expect("chunk loads");
        assert!(
            list.resident_chunks() <= 2,
            "at most two chunks resident, got {} at height {height}",
            list.resident_chunks(),
        );
    }

    // The frontier has moved past chunks 0 and 1.
    list.release_below(block::Height(300_000));
    assert_eq!(list.resident_chunks(), 1);

    list.release_below(block::Height(450_000));
    assert_eq!(list.resident_chunks(), 0);
}

#[test]
fn synthetic_list_round_trip() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");
    let hashes = fake_hashes(100);
    let spec = synthetic_spec(dir.path(), &hashes);

    let mut list = KnownHashList::open_spec(spec, &Network::Mainnet, Some(dir.path()))
        .expect("synthetic assets load");

    assert_eq!(list.max_height(), block::Height(99));
    for (i, expected) in hashes.iter().enumerate() {
        let actual = list
            .hash(block::Height(i as u32))
            .expect("single chunk loads")
            .expect("height is covered");
        assert_eq!(actual, *expected, "hash mismatch at height {i}");
    }
    assert!(list.hash(block::Height(100)).expect("no load").is_none());
}

#[test]
fn corrupt_chunk_rejected() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");
    let spec = synthetic_spec(dir.path(), &fake_hashes(100));

    // Flip one byte in the middle of the chunk file (not the genesis hash, so
    // the failure is the SHA-256 check, not the genesis check).
    let path = dir.path().join(spec.chunk_file_name(0));
    let mut bytes = std::fs::read(&path).expect("chunk readable");
    bytes[50 * 32] ^= 0x01;
    std::fs::write(&path, bytes).expect("chunk writable");

    let result = KnownHashList::open_spec(spec, &Network::Mainnet, Some(dir.path()));
    assert!(
        matches!(result, Err(KnownHashError::ChunkHashMismatch { .. })),
        "corrupt chunk must fail the SHA-256 check, got: {result:?}",
    );
}

#[test]
fn truncated_chunk_rejected() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");
    let spec = synthetic_spec(dir.path(), &fake_hashes(100));

    let path = dir.path().join(spec.chunk_file_name(0));
    let bytes = std::fs::read(&path).expect("chunk readable");
    // Truncate mid-hash: both misaligned and short.
    std::fs::write(&path, &bytes[..bytes.len() - 17]).expect("chunk writable");

    let result = KnownHashList::open_spec(spec, &Network::Mainnet, Some(dir.path()));
    assert!(
        matches!(result, Err(KnownHashError::ChunkLength { .. })),
        "truncated chunk must fail the length check, got: {result:?}",
    );
}

#[test]
fn genesis_mismatch_rejected() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");

    // A list whose height 0 is not the Mainnet genesis hash.
    let mut hashes = fake_hashes(100);
    hashes[0] = block::Hash([0xBB; 32]);
    let spec = synthetic_spec(dir.path(), &hashes);

    let result = KnownHashList::open_spec(spec, &Network::Mainnet, Some(dir.path()));
    assert!(
        matches!(result, Err(KnownHashError::GenesisMismatch { .. })),
        "wrong genesis must fail the genesis cross-check, got: {result:?}",
    );
}

#[test]
fn missing_assets_report_searched_dirs() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");
    // A spec whose chunk files exist nowhere (empty override dir, and the
    // prefix doesn't exist in any fallback directory either).
    let spec = synthetic_spec(dir.path(), &fake_hashes(10));
    std::fs::remove_file(dir.path().join(spec.chunk_file_name(0))).expect("file removable");

    let result = KnownHashList::open_spec(spec, &Network::Mainnet, Some(dir.path()));
    match result {
        Err(KnownHashError::AssetsNotFound { searched }) => {
            assert!(
                searched.first() == Some(&dir.path().to_owned()),
                "the config override must be searched first: {searched:?}",
            );
        }
        other => panic!("missing assets must report the search list, got: {other:?}"),
    }
}

#[test]
fn config_override_takes_priority() {
    let _init_guard = zebra_test::init();

    let dir = TempDir::new().expect("temp dir");
    let spec = synthetic_spec(dir.path(), &fake_hashes(10));

    // Resolution must pick the override directory (the only place the
    // synthetic chunk exists).
    let resolved = KnownHashList::resolve_dir(spec, Some(dir.path())).expect("assets resolve");
    assert_eq!(resolved, dir.path());
}
