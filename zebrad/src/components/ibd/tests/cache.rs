//! Fixed test vectors for the IBD disk overflow tier (`cache.rs`).
//!
//! Covers the §4.5 load-time reverification vectors: round-trip
//! preservation, restart-scan pruning (gate F10), corrupt and truncated
//! entries, and eviction.

use std::{fs, path::PathBuf, sync::Arc};

use tempfile::TempDir;

use zebra_chain::{
    block::{self, Block},
    serialization::ZcashSerialize,
};
use zebra_network::PeerSocketAddr;

use super::FakeHashList;
use crate::components::ibd::cache::{verify_entry, BlockCache, CACHE_DIR_NAME};

/// Returns a cache in a fresh temporary directory, the directory guard, and
/// the first `len` continuous mainnet blocks (block `i` is at height `i`).
fn cache_with_blocks(len: usize) -> (BlockCache, TempDir, Vec<Arc<Block>>) {
    let dir = TempDir::new().expect("test temp dirs are always creatable");
    let cache = BlockCache::new(dir.path().join(CACHE_DIR_NAME));
    let (_list, blocks) = FakeHashList::continuous_mainnet(len);

    (cache, dir, blocks)
}

/// Returns the cache directory under the test's temporary directory.
fn cache_dir(dir: &TempDir) -> PathBuf {
    dir.path().join(CACHE_DIR_NAME)
}

/// Returns the paths of the entry files currently on disk.
fn entry_files(dir: &TempDir) -> Vec<PathBuf> {
    match fs::read_dir(cache_dir(dir)) {
        Ok(entries) => entries
            .map(|entry| entry.expect("test dir entries are readable").path())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("test cache dir is readable: {error}"),
    }
}

/// `put` then `get` preserves the exact serialized bytes, the pinned hash,
/// and the source address (present, absent, and IPv6), and the bytes pass
/// the header-hash check.
#[test]
fn put_get_round_trip_preserves_bytes_and_source() {
    let _init_guard = zebra_test::init();
    let (mut cache, _dir, blocks) = cache_with_blocks(3);

    let v4_source: PeerSocketAddr = "203.0.113.7:8233".parse().expect("hardcoded addr parses");
    let v6_source: PeerSocketAddr = "[2001:db8::7]:8233".parse().expect("hardcoded addr parses");
    let sources = [Some(v4_source), None, Some(v6_source)];

    let mut expected_bytes = 0;
    for (height, (block, source)) in blocks.iter().zip(sources).enumerate() {
        // test heights are tiny, they fit u32
        let height = block::Height(height as u32);

        let raw = block
            .zcash_serialize_to_vec()
            .expect("serialization into a Vec never fails");

        let size = cache
            .put(height, block, source)
            .expect("writing to a fresh temp dir succeeds");
        assert_eq!(size as usize, raw.len(), "put returns the raw block size");
        expected_bytes += raw.len() as u64;

        let entry = cache
            .get(height)
            .expect("an entry that was just written is readable");
        assert_eq!(entry.bytes, raw, "raw block bytes round-trip exactly");
        assert_eq!(
            entry.expected_hash,
            block.hash(),
            "the pinned hash comes from the file name"
        );
        assert_eq!(entry.source, source, "the source address round-trips");

        assert!(
            verify_entry(&entry.bytes, entry.expected_hash),
            "an intact entry passes the header-hash check",
        );
    }

    assert_eq!(
        cache.bytes(),
        expected_bytes,
        "byte accounting sums raw block sizes"
    );
    assert_eq!(cache.len(), 3);
}

/// A restart `scan` prunes entries at or below the state tip and above the
/// list max (gate F10), deletes their files, and returns the survivors
/// sorted by height with their sizes; survivors keep their sources.
#[test]
fn scan_prunes_outside_range_and_returns_sorted_survivors() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(6);

    let source: PeerSocketAddr = "203.0.113.7:8233".parse().expect("hardcoded addr parses");
    for (height, block) in blocks.iter().enumerate() {
        // test heights are tiny, they fit u32
        let height = block::Height(height as u32);
        cache
            .put(height, block, Some(source))
            .expect("writing to a fresh temp dir succeeds");
    }

    // A fresh cache over the same directory simulates a restart.
    let mut restarted = BlockCache::new(cache_dir(&dir));
    let survivors = restarted
        .scan(Some(block::Height(1)), block::Height(4))
        .expect("scanning a populated cache dir succeeds");

    let expected: Vec<(block::Height, u32)> = (2..=4)
        .map(|height| {
            let raw_len = blocks[height]
                .zcash_serialize_to_vec()
                .expect("serialization into a Vec never fails")
                .len();
            // test blocks are far below u32::MAX bytes
            (block::Height(height as u32), raw_len as u32)
        })
        .collect();
    assert_eq!(
        survivors, expected,
        "survivors are (tip, list_max], sorted, with sizes"
    );

    assert_eq!(entry_files(&dir).len(), 3, "pruned entry files are deleted");
    assert_eq!(
        restarted.bytes(),
        expected
            .iter()
            .map(|(_, size)| u64::from(*size))
            .sum::<u64>(),
    );

    for height in [0, 1, 5] {
        assert!(
            restarted.get(block::Height(height)).is_none(),
            "pruned height {height} is gone",
        );
    }

    let entry = restarted
        .get(block::Height(2))
        .expect("a surviving entry is readable after restart");
    assert_eq!(
        entry.source,
        Some(source),
        "sources survive the restart scan"
    );
    assert!(verify_entry(&entry.bytes, entry.expected_hash));
}

/// With an empty state (no tip), `scan` keeps cached blocks from genesis up.
#[test]
fn scan_with_empty_state_keeps_genesis() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(2);

    for (height, block) in blocks.iter().enumerate() {
        // test heights are tiny, they fit u32
        cache
            .put(block::Height(height as u32), block, None)
            .expect("writing to a fresh temp dir succeeds");
    }

    let mut restarted = BlockCache::new(cache_dir(&dir));
    let survivors = restarted
        .scan(None, block::Height(1))
        .expect("scanning a populated cache dir succeeds");

    let heights: Vec<u32> = survivors.iter().map(|(height, _)| height.0).collect();
    assert_eq!(heights, [0, 1], "no tip means nothing is below the floor");
}

/// A corrupted entry body is still returned by `get` (the sidecar is
/// intact), but fails the header-hash check that promotion re-runs.
#[test]
fn corrupt_entry_fails_the_hash_check() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(1);

    cache
        .put(block::Height(0), &blocks[0], None)
        .expect("writing to a fresh temp dir succeeds");

    let files = entry_files(&dir);
    assert_eq!(files.len(), 1, "one block puts one file");

    // Flip a byte inside the stored header (10 bytes past the sidecar line
    // is within the previous-block-hash field, so the header still parses
    // but its hash changes).
    let mut file = fs::read(&files[0]).expect("the entry file is readable");
    let newline = file
        .iter()
        .position(|&byte| byte == b'\n')
        .expect("entries start with a one-line sidecar");
    file[newline + 1 + 10] ^= 0xff;
    fs::write(&files[0], &file).expect("the entry file is writable");

    let entry = cache
        .get(block::Height(0))
        .expect("a body-corrupt entry is returned: only re-verification can detect it");
    assert!(
        !verify_entry(&entry.bytes, entry.expected_hash),
        "the flipped byte must fail the header-hash check",
    );
}

/// Truncated entry files never panic: a file torn inside the sidecar is
/// dropped by `get` and pruned by `scan`; a file torn inside the body is
/// returned and fails verification.
#[test]
fn truncated_entries_are_dropped_or_fail_verification() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(2);

    for (height, block) in blocks.iter().enumerate() {
        // test heights are tiny, they fit u32
        cache
            .put(block::Height(height as u32), block, None)
            .expect("writing to a fresh temp dir succeeds");
    }

    let find_file = |height: u32| {
        entry_files(&dir)
            .into_iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!("{height}-")))
            })
            .expect("the entry file for the height exists")
    };
    let truncate = |height: u32, len: u64| {
        let path = find_file(height);
        fs::File::options()
            .write(true)
            .open(&path)
            .expect("the entry file is writable")
            .set_len(len)
            .expect("truncating a test file succeeds");
        path
    };

    // Torn inside the sidecar line: the entry is unusable, `get` drops it
    // and deletes the file.
    let torn_sidecar = truncate(0, 4);
    assert!(
        cache.get(block::Height(0)).is_none(),
        "an entry torn inside its sidecar is dropped",
    );
    assert!(!torn_sidecar.exists(), "the torn file is deleted");
    assert!(
        cache.get(block::Height(0)).is_none(),
        "the dropped entry stays gone",
    );

    // Torn inside the body: the sidecar parses, so the bytes come back, and
    // the truncated header fails verification downstream.
    let sidecar_len = {
        let file = fs::read(find_file(1)).expect("the entry file is readable");
        file.iter()
            .position(|&byte| byte == b'\n')
            .expect("entries start with a one-line sidecar") as u64
            + 1
    };
    truncate(1, sidecar_len + 100);

    let entry = cache
        .get(block::Height(1))
        .expect("a body-torn entry is returned: only re-verification can detect it");
    assert_eq!(entry.bytes.len(), 100);
    assert!(
        !verify_entry(&entry.bytes, entry.expected_hash),
        "a truncated header must fail the header-hash check",
    );

    // A restart scan prunes a sidecar-torn file instead of restoring it.
    truncate(1, 4);
    let mut restarted = BlockCache::new(cache_dir(&dir));
    let survivors = restarted
        .scan(None, block::Height(10))
        .expect("scanning a cache dir with torn entries succeeds");
    assert!(survivors.is_empty(), "scan prunes sidecar-torn entries");
    assert!(entry_files(&dir).is_empty(), "scan deletes pruned files");
}

/// `evict_through` deletes all entries at or below the committed height,
/// their files included, and leaves the rest readable.
#[test]
fn evict_through_deletes_committed_entries() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(5);

    for (height, block) in blocks.iter().enumerate() {
        // test heights are tiny, they fit u32
        cache
            .put(block::Height(height as u32), block, None)
            .expect("writing to a fresh temp dir succeeds");
    }

    cache.evict_through(block::Height(2));

    for height in 0..=2 {
        assert!(
            cache.get(block::Height(height)).is_none(),
            "evicted height {height} is gone",
        );
    }
    assert_eq!(
        entry_files(&dir).len(),
        2,
        "evicted entry files are deleted"
    );
    assert_eq!(cache.len(), 2);

    let expected_bytes: u64 = blocks[3..]
        .iter()
        .map(|block| {
            block
                .zcash_serialize_to_vec()
                .expect("serialization into a Vec never fails")
                .len() as u64
        })
        .sum();
    assert_eq!(cache.bytes(), expected_bytes);

    assert!(
        cache.get(block::Height(3)).is_some(),
        "surviving entries stay readable",
    );

    // Evicting far past the top clears everything else.
    cache.evict_through(block::Height(500));
    assert!(cache.is_empty());
    assert_eq!(cache.bytes(), 0);
    assert!(entry_files(&dir).is_empty());
}

/// `remove_all` deletes the entire cache directory for the `Completed`
/// handoff, and the cache stays usable afterwards.
#[test]
fn remove_all_deletes_the_cache_directory() {
    let _init_guard = zebra_test::init();
    let (mut cache, dir, blocks) = cache_with_blocks(1);

    cache
        .put(block::Height(0), &blocks[0], None)
        .expect("writing to a fresh temp dir succeeds");
    assert!(cache_dir(&dir).exists());

    cache
        .remove_all()
        .expect("removing an existing cache dir succeeds");
    assert!(!cache_dir(&dir).exists(), "the whole directory is removed");
    assert_eq!(cache.bytes(), 0);
    assert!(cache.get(block::Height(0)).is_none());

    // Removing an already-removed cache is fine, and writes recreate it.
    cache
        .remove_all()
        .expect("removing a missing cache dir is not an error");
    cache
        .put(block::Height(0), &blocks[0], None)
        .expect("the cache dir is recreated on the next write");
    assert!(cache.get(block::Height(0)).is_some());
}
