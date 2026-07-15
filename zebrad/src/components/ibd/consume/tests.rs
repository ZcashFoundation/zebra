//! Unit tests for the snapshot-consume read-and-verify helpers.
//!
//! These exercise the content-addressed verification gates (chunk SHA-256, tree
//! root vs the chunk's recorded root) without a live chain: the verification
//! logic is pure over bytes, and the artifact source is a temp directory laid
//! out like `emit-snapshot --emit-files` writes it.

use std::future::ready;

use bincode::Options;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tower::service_fn;

use zebra_chain::{
    block::Height,
    parameters::known_hashes::{
        chunk_v2::{self, ParsedChunk, TreeRoot},
        KnownHashListSpec, HASHES_PER_CHUNK,
    },
};
use zebra_state as zs;

use crate::components::ibd::engine::tree_fetch_stage;

use super::{
    local::{chunk_file_name, LocalSnapshotSource, LocalSourceError, CHUNKS_SUBDIR},
    *,
};

/// Builds a v2 chunk over `n` blocks with deterministic hashes/hints, and the
/// given sapling/orchard tree-root records, returning the encoded bytes.
fn build_chunk(n: u32, sapling: &[TreeRoot], orchard: &[TreeRoot]) -> Vec<u8> {
    let blocks: Vec<([u8; 32], u8)> = (0..n)
        .map(|i| {
            let mut hash = [0u8; 32];
            hash[0..4].copy_from_slice(&i.to_le_bytes());
            (hash, ((i % 254) + 1) as u8)
        })
        .collect();

    chunk_v2::encode(&blocks, true, sapling, orchard, true)
}

/// Leaks a single-chunk spec whose pinned hash matches `chunk_bytes`, so
/// `verify_chunk_bytes` accepts it.
fn leak_spec_for(chunk_bytes: &[u8], max_height: u32) -> &'static KnownHashListSpec {
    let chunk_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(chunk_bytes)).into_boxed_str());

    Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(max_height),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new([chunk_hash])),
        unspent_outputs_hash: None,
        address_balances_hash: None,
    }))
}

/// Writes a single chunk into `<dir>/chunks/chunk-<index>.bin`, mirroring the
/// emit layout.
fn write_chunk_file(dir: &std::path::Path, index: u32, bytes: &[u8]) {
    let chunks_dir = dir.join(CHUNKS_SUBDIR);
    std::fs::create_dir_all(&chunks_dir).expect("temp chunks dir is creatable");
    std::fs::write(chunks_dir.join(chunk_file_name(index)), bytes).expect("chunk file is writable");
}

/// Writes one `(height u32 LE, len u32 LE, frontier)` tree record file, mirroring
/// the emit layout, from ascending `(height, frontier-bytes)` records.
fn write_tree_records(path: &std::path::Path, records: &[(u32, Vec<u8>)]) {
    let mut out = Vec::new();
    for (height, frontier) in records {
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&(frontier.len() as u32).to_le_bytes());
        out.extend_from_slice(frontier);
    }
    std::fs::write(path, out).expect("tree records file is writable");
}

/// A local artifact source over an (initially empty) temp dir, returning the dir
/// guard so the caller can populate it.
fn empty_local_source() -> (TempDir, LocalSnapshotSource) {
    let dir = TempDir::new().expect("temp dir is creatable");
    let source = LocalSnapshotSource::new(dir.path());
    (dir, source)
}

#[test]
fn verify_chunk_bytes_accepts_matching_hash() {
    let _init = zebra_test::init();

    let chunk = build_chunk(5, &[], &[]);
    let spec = leak_spec_for(&chunk, 4);

    let verified = verify_chunk_bytes(spec, 0, chunk.clone()).expect("matching hash is accepted");
    assert_eq!(verified, chunk, "the verified bytes are the input bytes");

    // The verified bytes parse as a valid v2 chunk.
    let parsed = ParsedChunk::parse(&verified).expect("verified chunk parses");
    assert_eq!(parsed.block_count(), 5);
}

#[test]
fn verify_chunk_bytes_rejects_wrong_hash() {
    let _init = zebra_test::init();

    let chunk = build_chunk(5, &[], &[]);
    let spec = leak_spec_for(&chunk, 4);

    // Flip a hint byte so the bytes no longer match the pinned hash, but still
    // parse as a structurally valid chunk.
    let mut tampered = chunk.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;

    let error =
        verify_chunk_bytes(spec, 0, tampered).expect_err("a tampered chunk must be rejected");
    assert!(
        matches!(error, ConsumeError::ChunkHashMismatch { index: 0, .. }),
        "got {error:?}",
    );
}

#[test]
fn verify_chunk_bytes_rejects_out_of_range_index() {
    let _init = zebra_test::init();

    let chunk = build_chunk(3, &[], &[]);
    let spec = leak_spec_for(&chunk, 2);

    let error = verify_chunk_bytes(spec, 1, chunk).expect_err("index 1 has no pinned hash");
    assert!(
        matches!(error, ConsumeError::ChunkIndexOutOfRange { index: 1, .. }),
        "got {error:?}",
    );
}

/// `ensure_chunk` reads the chunk from the local state CF when it is present,
/// verifies it, and caches it without touching the artifact directory.
#[tokio::test]
async fn ensure_chunk_uses_local_cf_when_present() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    // The state serves the chunk from its CF; the artifact dir stays empty, so a
    // read from it would fail — proving the CF path never touches it.
    let stored = chunk.clone();
    let state = service_fn(move |req: zs::Request| {
        assert!(matches!(req, zs::Request::KnownHashChunk(0)));
        let bytes = stored.clone();
        ready(Ok::<_, BoxError>(zs::Response::KnownHashChunk(Some(bytes))))
    });
    let (_dir, source) = empty_local_source();

    let mut source = CfHashSource::new(spec, source, state);

    source
        .ensure_chunk(0)
        .await
        .expect("a locally-stored, valid chunk is accepted");
    assert!(
        source.has_chunk(0),
        "the chunk is cached after ensure_chunk"
    );
}

/// `ensure_chunk` reads the chunk from the artifact directory, verifies it,
/// persists it to the CF, and caches it when the CF lacks the chunk.
#[tokio::test]
async fn ensure_chunk_reads_verifies_and_persists_artifact_chunk() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    let dir = TempDir::new().expect("temp dir is creatable");
    write_chunk_file(dir.path(), 0, &chunk);

    // The state has no stored chunk on read, and accepts (and records) the
    // persist write. The verified chunk must be persisted back into the CF.
    let persisted = std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<u8>>));
    let persisted_in_service = persisted.clone();
    let state = service_fn(move |req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { index, bytes } => {
                assert_eq!(index, 0);
                *persisted_in_service.lock().unwrap() = Some(bytes);
                zs::Response::WroteKnownHashChunk
            }
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });

    let mut source = CfHashSource::new(spec, LocalSnapshotSource::new(dir.path()), state);

    source
        .ensure_chunk(0)
        .await
        .expect("a verified artifact chunk is accepted");
    assert!(source.has_chunk(0), "the artifact chunk is cached");

    // The verified bytes were persisted back into the CF.
    assert_eq!(
        persisted.lock().unwrap().as_deref(),
        Some(chunk.as_slice()),
        "the verified artifact chunk is persisted to the state CF",
    );

    // The cached chunk now backs the synchronous hash lookup.
    let hash = HashSource::hash(&mut source, Height(0)).expect("hash lookup succeeds");
    assert!(hash.is_some(), "the cached chunk serves height 0's hash");
}

/// An artifact chunk file whose bytes do not hash to the pinned constant is
/// rejected with the precise hash-mismatch error, never trusting the wrong
/// bytes.
#[tokio::test]
async fn ensure_chunk_rejects_wrong_hash_artifact_chunk() {
    let _init = zebra_test::init();

    let chunk = build_chunk(6, &[], &[]);
    let spec = leak_spec_for(&chunk, 5);

    // Write a TAMPERED chunk file (still a valid v2 chunk structurally, but its
    // SHA-256 no longer matches the pinned hash).
    let mut tampered = chunk.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;

    let dir = TempDir::new().expect("temp dir is creatable");
    write_chunk_file(dir.path(), 0, &tampered);

    let state = service_fn(|req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });

    let mut source = CfHashSource::new(spec, LocalSnapshotSource::new(dir.path()), state);

    let error = source
        .ensure_chunk(0)
        .await
        .expect_err("a wrong-hash artifact chunk must be rejected");
    assert!(
        matches!(error, ConsumeError::ChunkHashMismatch { index: 0, .. }),
        "got {error:?}",
    );
    assert!(!source.has_chunk(0), "a wrong-hash chunk is never cached");

    // A chunk with no artifact file at all surfaces the precise missing-path
    // error, not a generic failure.
    std::fs::remove_file(dir.path().join(CHUNKS_SUBDIR).join(chunk_file_name(0)))
        .expect("the tampered chunk file exists and is removable");
    let error = source
        .ensure_chunk(0)
        .await
        .expect_err("a missing artifact chunk file must be an error");
    assert!(
        matches!(
            error,
            ConsumeError::LocalSource(LocalSourceError::Missing { .. })
        ),
        "got {error:?}",
    );
}

/// `ensure_covers` primes every chunk covering the window, including a second
/// chunk the frontier has advanced into — so the synchronous `hash` lookup keeps
/// working across a chunk boundary (the #11 regression).
#[tokio::test]
async fn ensure_covers_primes_across_chunk_boundary() {
    use crate::components::ibd::engine::HashSource;

    let _init = zebra_test::init();

    // Two full chunks: index 0 covers [0, HASHES_PER_CHUNK), index 1 covers
    // [HASHES_PER_CHUNK, 2*HASHES_PER_CHUNK). Distinct contents so each has a
    // distinct pinned hash.
    let chunk_0 = build_chunk(HASHES_PER_CHUNK, &[], &[]);
    let mut chunk_1 = build_chunk(HASHES_PER_CHUNK, &[], &[]);
    // Perturb chunk 1's first hash byte so it differs from chunk 0.
    chunk_1[16] ^= 0x5A;

    let hash_0: &'static str = Box::leak(hex::encode(Sha256::digest(&chunk_0)).into_boxed_str());
    let hash_1: &'static str = Box::leak(hex::encode(Sha256::digest(&chunk_1)).into_boxed_str());

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(2 * HASHES_PER_CHUNK - 1),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new([hash_0, hash_1])),
        unspent_outputs_hash: None,
        address_balances_hash: None,
    }));

    // The state has no stored chunks and accepts persists; the artifact dir
    // holds both chunk files.
    let state = service_fn(|req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });
    let dir = TempDir::new().expect("temp dir is creatable");
    write_chunk_file(dir.path(), 0, &chunk_0);
    write_chunk_file(dir.path(), 1, &chunk_1);

    let mut source = CfHashSource::new(spec, LocalSnapshotSource::new(dir.path()), state);

    // Prime only the first chunk (the bootstrap window).
    source
        .ensure_covers(Height(0), Height(0))
        .await
        .expect("the first chunk primes");
    assert!(source.has_chunk(0), "chunk 0 is primed");
    assert!(!source.has_chunk(1), "chunk 1 is not yet primed");

    // A height in chunk 1 has no resident chunk: the synchronous lookup errors
    // (no v1 fallback), which is the condition the engine re-primes against.
    assert!(
        HashSource::hash(&mut source, Height(HASHES_PER_CHUNK)).is_err(),
        "a height in the unprimed chunk 1 errors before re-priming",
    );

    // As the frontier advances into chunk 1, `ensure_covers` over the new window
    // primes chunk 1 too.
    source
        .ensure_covers(Height(HASHES_PER_CHUNK), Height(HASHES_PER_CHUNK))
        .await
        .expect("the second chunk primes as the frontier advances");
    assert!(source.has_chunk(1), "chunk 1 is primed after ensure_covers");

    // The lookup now works for the height in chunk 1.
    let hash = HashSource::hash(&mut source, Height(HASHES_PER_CHUNK))
        .expect("the lookup succeeds after re-priming")
        .expect("chunk 1 covers this height");
    assert_eq!(
        hash.0,
        ParsedChunk::parse(&chunk_1)
            .expect("chunk 1 parses")
            .block_hash(0),
        "the re-primed chunk 1 serves the correct hash across the boundary",
    );
}

#[test]
fn chunk_index_and_span_base_round_trip() {
    assert_eq!(chunk_index_for_height(Height(0)), 0);
    assert_eq!(chunk_index_for_height(Height(HASHES_PER_CHUNK - 1)), 0);
    assert_eq!(chunk_index_for_height(Height(HASHES_PER_CHUNK)), 1);
    assert_eq!(chunk_index_for_height(Height(HASHES_PER_CHUNK * 3 + 7)), 3);

    assert_eq!(chunk_span_base(0), Some(0));
    assert_eq!(chunk_span_base(2), Some(2 * HASHES_PER_CHUNK));
    // A huge index overflows the span-base multiply and fails safe.
    assert_eq!(chunk_span_base(u32::MAX), None);
}

/// Primes a chunk into a `CfHashSource`'s cache from already-verified bytes, for
/// the tree-lookahead source tests (the chunk is served from the local "CF").
/// Returns the primed source as an `impl HashSource`, since the tests only
/// exercise the trait methods.
async fn primed_source(chunk_bytes: Vec<u8>, max_height: u32) -> impl HashSource {
    let spec = leak_spec_for(&chunk_bytes, max_height);

    // Serve the chunk from the local "CF" so `ensure_chunk` caches it; the
    // persist write is accepted but ignored.
    let stored = chunk_bytes.clone();
    let state = service_fn(move |req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(Some(stored.clone())),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });
    // The artifact directory is never read (the "CF" serves the chunk), so a
    // nonexistent path proves it.
    let source = LocalSnapshotSource::new("/nonexistent-snapshot-artifact-dir");

    let mut source = CfHashSource::new(spec, source, state);

    source
        .ensure_chunk(0)
        .await
        .expect("the primed chunk is cached");
    source
}

/// `tree_updates_in` maps the chunk's sparse `rel_height` records to absolute
/// updating heights within the window, paired with their pool, ascending.
#[tokio::test]
async fn tree_updates_in_maps_sparse_records_to_absolute_heights() {
    use crate::components::ibd::engine::HashSource;
    use crate::components::ibd::tree::TreeFetch;

    let _init = zebra_test::init();

    // A chunk over 200 blocks: sapling updates at rel 10 and 120, orchard at
    // rel 10 and 57. Chunk 0, so absolute height == rel height.
    let chunk = build_chunk(
        200,
        &[
            TreeRoot {
                rel_height: 10,
                root: [1u8; 32],
            },
            TreeRoot {
                rel_height: 120,
                root: [2u8; 32],
            },
        ],
        &[
            TreeRoot {
                rel_height: 10,
                root: [3u8; 32],
            },
            TreeRoot {
                rel_height: 57,
                root: [4u8; 32],
            },
        ],
    );
    let mut source = primed_source(chunk, 199).await;

    // A window that includes 10 and 57 but excludes 120.
    let updates = source.tree_updates_in(Height(0), Height(60));
    assert_eq!(
        updates,
        vec![
            TreeFetch {
                height: Height(10),
                pool: ShieldedPool::Sapling
            },
            TreeFetch {
                height: Height(10),
                pool: ShieldedPool::Orchard
            },
            TreeFetch {
                height: Height(57),
                pool: ShieldedPool::Orchard
            },
        ],
        "only updating heights inside the window are reported, ascending",
    );

    // A narrow window past the last in-range update reports nothing.
    assert!(
        source.tree_updates_in(Height(58), Height(119)).is_empty(),
        "no updating heights between 58 and 119",
    );
}

/// `tree_root` returns the exact recorded root for an updating height, and
/// `None` for a non-updating height (so the lookahead never verifies against a
/// stale earlier root).
#[tokio::test]
async fn tree_root_returns_exact_recorded_root() {
    use crate::components::ibd::engine::HashSource;

    let _init = zebra_test::init();

    let chunk = build_chunk(
        200,
        &[TreeRoot {
            rel_height: 10,
            root: [7u8; 32],
        }],
        &[],
    );
    let mut source = primed_source(chunk, 199).await;

    assert_eq!(
        source.tree_root(ShieldedPool::Sapling, Height(10)),
        Some([7u8; 32]),
        "the exact recorded root is returned for the updating height",
    );
    assert_eq!(
        source.tree_root(ShieldedPool::Sapling, Height(11)),
        None,
        "a non-updating height returns None, not the earlier root",
    );
    assert_eq!(
        source.tree_root(ShieldedPool::Orchard, Height(10)),
        None,
        "a pool with no record at the height returns None",
    );
}

// --- Tree-fetch stage tests ---------------------------------------------------
//
// The engine's `tree_fetch_stage` reads a tree from the artifact directory and
// verifies its deserialized `.root()` against the root the chunk records. These
// prove the accept and every reject/fold path over real serialized trees.

/// Serializes a default sapling tree, returning its bytes and real root.
fn sapling_tree_bytes_and_root() -> (Vec<u8>, [u8; 32]) {
    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let root: [u8; 32] = (&tree.root()).into();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");
    (tree_bytes, root)
}

/// A tree whose recomputed root matches the expected (chunk-recorded) root is
/// accepted, yielding the exact artifact bytes.
#[tokio::test]
async fn tree_fetch_stage_accepts_matching_root() {
    use super::local::SAPLING_TREES_FILE;

    let _init = zebra_test::init();

    let (tree_bytes, root) = sapling_tree_bytes_and_root();

    let dir = TempDir::new().expect("temp dir is creatable");
    write_tree_records(
        &dir.path().join(SAPLING_TREES_FILE),
        &[(0, tree_bytes.clone())],
    );

    let local = LocalSnapshotSource::new(dir.path());
    let verified = tree_fetch_stage(Some(local), ShieldedPool::Sapling, Height(0), root)
        .await
        .expect("a tree matching the recorded root is accepted");
    assert_eq!(
        verified, tree_bytes,
        "the verified bytes are the file bytes"
    );
}

/// A tree whose root disagrees with the recorded root, undeserializable tree
/// bytes, and a missing record all fold (return `None`) — never trusting
/// unverified bytes, never fatal. (A missing *source* is a wiring bug caught
/// by a debug assertion, so it is not exercised here.)
#[tokio::test]
async fn tree_fetch_stage_folds_on_mismatch_garbage_or_missing() {
    use super::local::SAPLING_TREES_FILE;

    let _init = zebra_test::init();

    let (tree_bytes, root) = sapling_tree_bytes_and_root();
    let wrong_root = [0x11u8; 32];

    // A real tree against a wrong recorded root: fold.
    //
    // Each sub-case constructs a fresh source: the source caches the parsed
    // records file on first read (the artifact directory is read-only in
    // production), so a rewritten file needs a fresh instance to be seen.
    let dir = TempDir::new().expect("temp dir is creatable");
    write_tree_records(&dir.path().join(SAPLING_TREES_FILE), &[(0, tree_bytes)]);
    assert_eq!(
        tree_fetch_stage(
            Some(LocalSnapshotSource::new(dir.path())),
            ShieldedPool::Sapling,
            Height(0),
            wrong_root,
        )
        .await,
        None,
        "a tree whose root disagrees with the chunk folds",
    );

    // Garbage bytes that do not deserialize: fold.
    write_tree_records(
        &dir.path().join(SAPLING_TREES_FILE),
        &[(0, vec![0xFFu8; 7])],
    );
    assert_eq!(
        tree_fetch_stage(
            Some(LocalSnapshotSource::new(dir.path())),
            ShieldedPool::Sapling,
            Height(0),
            root,
        )
        .await,
        None,
        "undeserializable tree bytes fold",
    );

    // A height with no record in the file: fold.
    assert_eq!(
        tree_fetch_stage(
            Some(LocalSnapshotSource::new(dir.path())),
            ShieldedPool::Sapling,
            Height(99),
            root,
        )
        .await,
        None,
        "a missing tree record folds",
    );
}

/// The `LocalSnapshotSource` reader round-trips multi-record tree files and
/// reports a malformed (truncated) file as an error.
#[test]
fn local_snapshot_source_reads_tree_records() {
    use super::local::SAPLING_TREES_FILE;

    let _init = zebra_test::init();

    let dir = TempDir::new().expect("temp dir is creatable");
    let records = vec![
        (3u32, vec![0xAAu8; 5]),
        (10u32, vec![0xBBu8; 0]),
        (42u32, vec![0xCCu8; 9]),
    ];
    write_tree_records(&dir.path().join(SAPLING_TREES_FILE), &records);

    let source = LocalSnapshotSource::new(dir.path());
    let all = source
        .read_all_trees(ShieldedPool::Sapling)
        .expect("the records file parses");
    assert_eq!(all.len(), 3, "all three records are read");
    assert_eq!(all.get(&3).map(Vec::as_slice), Some([0xAAu8; 5].as_slice()));
    assert_eq!(all.get(&10).map(Vec::as_slice), Some([].as_slice()));
    assert_eq!(
        all.get(&42).map(Vec::as_slice),
        Some([0xCCu8; 9].as_slice())
    );

    // A non-recorded height is `None` (the engine folds), not an error.
    assert_eq!(
        source
            .read_tree(ShieldedPool::Sapling, 7)
            .expect("a missing height is not an error"),
        None,
    );

    // A truncated header is a hard malformed-file error.
    std::fs::write(dir.path().join(SAPLING_TREES_FILE), [0u8; 3])
        .expect("truncated file is writable");
    let error = LocalSnapshotSource::new(dir.path())
        .read_all_trees(ShieldedPool::Sapling)
        .expect_err("a truncated tree file is malformed");
    assert!(
        matches!(error, LocalSourceError::MalformedTrees { .. }),
        "got {error:?}",
    );
}
