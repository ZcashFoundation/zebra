//! Unit tests for the snapshot-consume fetch-and-verify helpers.
//!
//! These exercise the content-addressed verification gates (chunk SHA-256, tree
//! root vs the chunk's recorded root, snapshot-set SHA-256) without a live chain
//! or peers: the verification logic is pure over bytes.

use std::future::ready;

use bincode::Options;
use sha2::{Digest, Sha256};
use tower::service_fn;

use zebra_chain::{
    block::Height,
    parameters::known_hashes::{
        chunk_v2::{self, ParsedChunk, TreeRoot},
        KnownHashListSpec, HASHES_PER_CHUNK,
    },
};
use zebra_network::{self as zn, ShieldedPool};
use zebra_state as zs;

use super::*;

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

#[test]
fn verify_tree_root_accepts_matching_root() {
    let _init = zebra_test::init();

    // Build a tree, record its real root in the chunk, and verify the same
    // serialized tree against it.
    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let root: [u8; 32] = (&tree.root()).into();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");

    // Record the root at rel height 0; the chunk covers heights [span_base..].
    let chunk = build_chunk(
        2,
        &[TreeRoot {
            rel_height: 0,
            root,
        }],
        &[],
    );
    let parsed = ParsedChunk::parse(&chunk).expect("chunk parses");

    let span_base = 0;
    let verified = verify_tree_root(
        ShieldedPool::Sapling,
        Height(0),
        span_base,
        &parsed,
        tree_bytes.clone(),
    )
    .expect("a tree matching the recorded root is accepted");
    assert_eq!(
        verified, tree_bytes,
        "the verified bytes are the input bytes"
    );
}

#[test]
fn verify_tree_root_rejects_wrong_root() {
    let _init = zebra_test::init();

    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");

    // Record a deliberately wrong root in the chunk.
    let wrong_root = [0x11u8; 32];
    let chunk = build_chunk(
        2,
        &[TreeRoot {
            rel_height: 0,
            root: wrong_root,
        }],
        &[],
    );
    let parsed = ParsedChunk::parse(&chunk).expect("chunk parses");

    let error = verify_tree_root(ShieldedPool::Sapling, Height(0), 0, &parsed, tree_bytes)
        .expect_err("a tree whose root disagrees with the chunk must be rejected");
    assert!(
        matches!(error, ConsumeError::TreeRootMismatch { .. }),
        "got {error:?}",
    );
}

#[test]
fn verify_tree_root_rejects_undeserializable_bytes() {
    let _init = zebra_test::init();

    let chunk = build_chunk(
        2,
        &[TreeRoot {
            rel_height: 0,
            root: [0u8; 32],
        }],
        &[],
    );
    let parsed = ParsedChunk::parse(&chunk).expect("chunk parses");

    let garbage = vec![0xFFu8; 7];
    let error = verify_tree_root(ShieldedPool::Sapling, Height(0), 0, &parsed, garbage)
        .expect_err("undeserializable tree bytes must be rejected");
    assert!(
        matches!(error, ConsumeError::TreeDeserialize { .. }),
        "got {error:?}",
    );
}

#[test]
fn verify_tree_root_errors_without_recorded_root() {
    let _init = zebra_test::init();

    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");

    // No sapling roots recorded in the chunk at all.
    let chunk = build_chunk(2, &[], &[]);
    let parsed = ParsedChunk::parse(&chunk).expect("chunk parses");

    let error = verify_tree_root(ShieldedPool::Sapling, Height(1), 0, &parsed, tree_bytes)
        .expect_err("no recorded root means the tree cannot be verified");
    assert!(
        matches!(error, ConsumeError::NoRecordedTreeRoot { .. }),
        "got {error:?}",
    );
}

#[test]
fn verify_set_hash_matches_and_mismatches() {
    let _init = zebra_test::init();

    let set_bytes = vec![0xABu8; 64];
    let set_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(&set_bytes)).into_boxed_str());

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some(set_hash),
        address_balances_hash: None,
    }));

    let verified = verify_set_hash(spec, "unspent-output", set_bytes.clone())
        .expect("a matching set is accepted");
    assert_eq!(verified, set_bytes);

    let mut tampered = set_bytes.clone();
    tampered[0] ^= 0xFF;
    let error = verify_set_hash(spec, "unspent-output", tampered)
        .expect_err("a tampered set must be rejected");
    assert!(
        matches!(error, ConsumeError::SetHashMismatch { .. }),
        "got {error:?}",
    );
}

#[test]
fn verify_set_hash_errors_when_not_pinned() {
    let _init = zebra_test::init();

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: None,
        address_balances_hash: None,
    }));

    let error = verify_set_hash(spec, "address-balance", vec![0u8; 8])
        .expect_err("an unpinned set cannot be verified");
    assert!(
        matches!(
            error,
            ConsumeError::SetHashNotPinned {
                set: "address-balance"
            }
        ),
        "got {error:?}",
    );
}

/// Serializes a default sapling tree and records its real root in a chunk at
/// rel height 0, returning the tree bytes and the parsed chunk's owned bytes.
fn sapling_tree_and_chunk() -> (Vec<u8>, Vec<u8>) {
    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let root: [u8; 32] = (&tree.root()).into();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");

    let chunk = build_chunk(
        2,
        &[TreeRoot {
            rel_height: 0,
            root,
        }],
        &[],
    );
    (tree_bytes, chunk)
}

#[tokio::test]
async fn fetch_and_verify_tree_accepts_matching_peer_tree() {
    let _init = zebra_test::init();

    let (tree_bytes, chunk_bytes) = sapling_tree_and_chunk();
    let parsed = ParsedChunk::parse(&chunk_bytes).expect("chunk parses");

    // A peer that serves the matching tree.
    let served = tree_bytes.clone();
    let peer_set = service_fn(move |req: zn::Request| {
        assert!(matches!(req, zn::Request::NoteCommitmentTree { .. }));
        let bytes = served.clone();
        ready(Ok::<_, BoxError>(zn::Response::NoteCommitmentTree(
            bytes.into(),
        )))
    });

    let source = SnapshotSource::p2p(peer_set);
    let verified = fetch_and_verify_tree(&source, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
        .await
        .expect("a matching peer tree is accepted");
    assert_eq!(verified, tree_bytes);
}

#[tokio::test]
async fn fetch_and_verify_tree_retries_past_bad_peer() {
    let _init = zebra_test::init();

    let (tree_bytes, chunk_bytes) = sapling_tree_and_chunk();
    let parsed = ParsedChunk::parse(&chunk_bytes).expect("chunk parses");

    // First peer serves garbage; the second serves the real tree. The retry loop
    // must skip the bad peer and accept the good one.
    let good = tree_bytes.clone();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let peer_set = service_fn(move |_req: zn::Request| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let bytes: Vec<u8> = if n == 0 { vec![0xFF; 7] } else { good.clone() };
        ready(Ok::<_, BoxError>(zn::Response::NoteCommitmentTree(
            bytes.into(),
        )))
    });

    let source = SnapshotSource::p2p(peer_set);
    let verified = fetch_and_verify_tree(&source, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
        .await
        .expect("the second peer's tree is accepted");
    assert_eq!(verified, tree_bytes);
}

#[tokio::test]
async fn fetch_and_verify_tree_unavailable_after_all_notfound() {
    let _init = zebra_test::init();

    let (_tree_bytes, chunk_bytes) = sapling_tree_and_chunk();
    let parsed = ParsedChunk::parse(&chunk_bytes).expect("chunk parses");

    // Every peer answers NotFound.
    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));

    let source = SnapshotSource::p2p(peer_set);
    let error = fetch_and_verify_tree(&source, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
        .await
        .expect_err("no peer served the tree");
    assert!(
        matches!(error, ConsumeError::Unavailable { .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn fetch_and_verify_set_assembles_and_verifies() {
    let _init = zebra_test::init();

    // A set whose bytes are served in ranges; the assembled hash must match.
    let set_bytes: Vec<u8> = (0..200u32).flat_map(|i| i.to_le_bytes()).collect();
    let set_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(&set_bytes)).into_boxed_str());

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some(set_hash),
        address_balances_hash: None,
    }));

    let served = set_bytes.clone();
    let peer_set = service_fn(move |req: zn::Request| {
        let (offset, len) = match req {
            zn::Request::UnspentOutputs { offset, len } => (offset as usize, len as usize),
            other => panic!("unexpected request {other:?}"),
        };
        let end = (offset + len).min(served.len());
        let slice = served.get(offset..end).unwrap_or(&[]).to_vec();
        ready(Ok::<_, BoxError>(zn::Response::SnapshotRange(slice.into())))
    });

    let source = SnapshotSource::p2p(peer_set);
    let verified = fetch_and_verify_set(
        &source,
        spec,
        "unspent-output",
        local::UNSPENT_OUTPUTS_FILE,
        set_bytes.len() as u64,
        zs::UNSPENT_OUTPUT_RECORD_LEN,
        3,
        |offset, len| zn::Request::UnspentOutputs { offset, len },
    )
    .await
    .expect("the assembled set verifies against the pinned hash");
    assert_eq!(verified, set_bytes);
}

#[tokio::test]
async fn fetch_and_verify_set_rejects_tampered_assembly() {
    let _init = zebra_test::init();

    let set_bytes: Vec<u8> = vec![0x42; 128];
    let set_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(&set_bytes)).into_boxed_str());

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some(set_hash),
        address_balances_hash: None,
    }));

    // A peer that serves a byte-flipped set: assembly succeeds, but the hash
    // check must reject it.
    let peer_set = service_fn(move |req: zn::Request| {
        let len = match req {
            zn::Request::UnspentOutputs { len, .. } => len as usize,
            other => panic!("unexpected request {other:?}"),
        };
        ready(Ok::<_, BoxError>(zn::Response::SnapshotRange(
            vec![0x43; len].into(),
        )))
    });

    let source = SnapshotSource::p2p(peer_set);
    let error = fetch_and_verify_set(
        &source,
        spec,
        "unspent-output",
        local::UNSPENT_OUTPUTS_FILE,
        set_bytes.len() as u64,
        zs::UNSPENT_OUTPUT_RECORD_LEN,
        3,
        |offset, len| zn::Request::UnspentOutputs { offset, len },
    )
    .await
    .expect_err("a tampered set must be rejected");
    assert!(
        matches!(error, ConsumeError::SetHashMismatch { .. }),
        "got {error:?}"
    );
}

/// `fetch_and_verify_set` refuses an over-large caller-supplied `total_len`
/// before allocating or doing any network work.
#[tokio::test]
async fn fetch_and_verify_set_rejects_oversized_total_len() {
    let _init = zebra_test::init();

    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some("00"),
        address_balances_hash: None,
    }));

    // The peer set must never be asked: the oversized length is refused first.
    let peer_set = service_fn(|_req: zn::Request| {
        panic!("the peer set must not be asked for an oversized set");
        #[allow(unreachable_code)]
        ready(Ok::<_, BoxError>(zn::Response::NotFound))
    });

    let source = SnapshotSource::p2p(peer_set);
    let error = fetch_and_verify_set(
        &source,
        spec,
        "unspent-output",
        local::UNSPENT_OUTPUTS_FILE,
        u64::MAX,
        zs::UNSPENT_OUTPUT_RECORD_LEN,
        3,
        |offset, len| zn::Request::UnspentOutputs { offset, len },
    )
    .await
    .expect_err("an over-large total_len must be refused before allocating");
    assert!(
        matches!(error, ConsumeError::SetTooLarge { .. }),
        "got {error:?}"
    );
}

/// `ensure_chunk` reads the chunk from the local state CF when it is present,
/// verifies it, and caches it without a network round-trip.
#[tokio::test]
async fn ensure_chunk_uses_local_cf_when_present() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    // The state serves the chunk from its CF; the peer set must never be called.
    let stored = chunk.clone();
    let state = service_fn(move |req: zs::Request| {
        assert!(matches!(req, zs::Request::KnownHashChunk(0)));
        let bytes = stored.clone();
        ready(Ok::<_, BoxError>(zs::Response::KnownHashChunk(Some(bytes))))
    });
    let peer_set = service_fn(|_req: zn::Request| {
        panic!("the peer set must not be asked when the CF has the chunk");
        #[allow(unreachable_code)]
        ready(Ok::<_, BoxError>(zn::Response::NotFound))
    });

    let mut source = CfHashSource::new(spec, SnapshotSource::p2p(peer_set), state);

    source
        .ensure_chunk(0, 3)
        .await
        .expect("a locally-stored, valid chunk is accepted");
    assert!(
        source.has_chunk(0),
        "the chunk is cached after ensure_chunk"
    );
}

/// `ensure_chunk` fetches from a peer, verifies, persists, and caches when the
/// CF lacks the chunk.
#[tokio::test]
async fn ensure_chunk_fetches_and_verifies_from_peer() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

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
    let served = chunk.clone();
    let peer_set = service_fn(move |req: zn::Request| {
        // The chunk is fetched ranged; serve the requested byte window of the
        // deterministic chunk bytes (the whole small chunk fits one range, so a
        // single offset-0 request returns a short final range).
        let zn::Request::KnownHashChunkRange { index, offset, len } = req else {
            panic!("expected a KnownHashChunkRange request, got {req:?}");
        };
        assert_eq!(index, 0);
        let start = offset as usize;
        let end = (offset.saturating_add(u64::from(len)) as usize).min(served.len());
        let range = served.get(start..end).unwrap_or_default().to_vec();
        ready(Ok::<_, BoxError>(zn::Response::SnapshotRange(range.into())))
    });

    let mut source = CfHashSource::new(spec, SnapshotSource::p2p(peer_set), state);

    source
        .ensure_chunk(0, 3)
        .await
        .expect("a verified peer chunk is accepted");
    assert!(source.has_chunk(0), "the peer chunk is cached");

    // The verified bytes were persisted back into the CF.
    assert_eq!(
        persisted.lock().unwrap().as_deref(),
        Some(chunk.as_slice()),
        "the verified peer chunk is persisted to the state CF",
    );

    // The cached chunk now backs the synchronous hash lookup.
    let hash = HashSource::hash(&mut source, Height(0)).expect("hash lookup succeeds");
    assert!(hash.is_some(), "the cached chunk serves height 0's hash");
}

/// `ensure_chunk` tolerates a transient peer-set failure on one attempt and
/// keeps trying other peers up to the attempt cap.
#[tokio::test]
async fn ensure_chunk_tolerates_transient_peer_failure() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    // The state has no stored chunk and accepts the persist.
    let state = service_fn(|req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });

    // The first peer-set call fails outright (a transient peer-set error); the
    // second serves the matching chunk. A single transient failure must not
    // abort the whole prime.
    let served = chunk.clone();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let peer_set = service_fn(move |req: zn::Request| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let zn::Request::KnownHashChunkRange { offset, len, .. } = req else {
            panic!("expected a KnownHashChunkRange request, got {req:?}");
        };
        if n == 0 {
            return ready(Err::<zn::Response, BoxError>(
                "transient peer-set failure".into(),
            ));
        }
        let start = offset as usize;
        let end = (offset.saturating_add(u64::from(len)) as usize).min(served.len());
        let range = served.get(start..end).unwrap_or_default().to_vec();
        ready(Ok::<_, BoxError>(zn::Response::SnapshotRange(range.into())))
    });

    let mut source = CfHashSource::new(spec, SnapshotSource::p2p(peer_set), state);

    source
        .ensure_chunk(0, 4)
        .await
        .expect("a transient peer failure is tolerated and a later peer succeeds");
    assert!(
        source.has_chunk(0),
        "the chunk is cached after retrying past the transient failure",
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

    // The state has no stored chunks and accepts persists; peers serve the
    // requested chunk's ranges by index.
    let state = service_fn(|req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });
    let served_0 = chunk_0.clone();
    let served_1 = chunk_1.clone();
    let peer_set = service_fn(move |req: zn::Request| {
        let zn::Request::KnownHashChunkRange { index, offset, len } = req else {
            panic!("expected a KnownHashChunkRange request, got {req:?}");
        };
        let served: &[u8] = match index {
            0 => &served_0,
            1 => &served_1,
            other => panic!("unexpected chunk index {other}"),
        };
        let start = offset as usize;
        let end = (offset.saturating_add(u64::from(len)) as usize).min(served.len());
        let range = served.get(start..end).unwrap_or_default().to_vec();
        ready(Ok::<_, BoxError>(zn::Response::SnapshotRange(range.into())))
    });

    let mut source = CfHashSource::new(spec, SnapshotSource::p2p(peer_set), state);

    // Prime only the first chunk (the bootstrap window).
    source
        .ensure_covers(Height(0), Height(0), 3)
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
        .ensure_covers(Height(HASHES_PER_CHUNK), Height(HASHES_PER_CHUNK), 3)
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
/// the tree-lookahead source tests, without a network round-trip (the chunk is
/// served from the local "CF"). Returns the primed source as an `impl
/// HashSource`, since the tests only exercise the trait methods.
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
    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));

    let mut source = CfHashSource::new(spec, SnapshotSource::p2p(peer_set), state);

    source
        .ensure_chunk(0, 1)
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

// --- Local-file source round-trip tests --------------------------------------
//
// These prove that the local-file `SnapshotSource` reads and verifies each
// artifact identically to the P2P path: it yields the same chunk / tree / set
// bytes (so the same content-addressed checks pass), and rejects a wrong-hash
// file exactly as a corrupt peer is rejected. They write artifacts in the same
// layout `emit-snapshot --emit-files` produces, then read them back through the
// local source — the solo-sync test path.

use tempfile::TempDir;

use local::{
    chunk_file_name, LocalSnapshotSource, ADDRESS_BALANCES_FILE, CHUNKS_SUBDIR, SAPLING_TREES_FILE,
    UNSPENT_OUTPUTS_FILE,
};

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

/// The local-file source reassembles a chunk identical to the P2P path and the
/// CF-backed source caches it, so the synchronous `hash` lookup works — with the
/// peer set never asked.
#[tokio::test]
async fn local_source_chunk_matches_p2p_and_caches() {
    let _init = zebra_test::init();

    let chunk = build_chunk(6, &[], &[]);
    let spec = leak_spec_for(&chunk, 5);

    let dir = TempDir::new().expect("temp dir is creatable");
    write_chunk_file(dir.path(), 0, &chunk);

    // The CF is empty (so the local file is read), and the persist is accepted.
    let state = service_fn(|req: zs::Request| {
        let response = match req {
            zs::Request::KnownHashChunk(_) => zs::Response::KnownHashChunk(None),
            zs::Request::WriteKnownHashChunk { .. } => zs::Response::WroteKnownHashChunk,
            other => panic!("unexpected state request {other:?}"),
        };
        ready(Ok::<_, BoxError>(response))
    });
    // The peer set must never be asked: the local file backs the chunk.
    let peer_set = service_fn(|_req: zn::Request| {
        panic!("the peer set must not be asked when a local chunk file exists");
        #[allow(unreachable_code)]
        ready(Ok::<_, BoxError>(zn::Response::NotFound))
    });

    let source = SnapshotSource::local_files(dir.path(), peer_set);
    assert!(source.is_local(), "the source reads local files");
    let mut hash_source = CfHashSource::new(spec, source, state);

    hash_source
        .ensure_chunk(0, 3)
        .await
        .expect("the local chunk file is read and verified");
    assert!(hash_source.has_chunk(0), "the local chunk is cached");

    // The cached chunk serves the same hash the P2P path would (the bytes are
    // identical, so the parse yields the same block hash).
    let hash = HashSource::hash(&mut hash_source, Height(0))
        .expect("the lookup succeeds")
        .expect("chunk 0 covers height 0");
    assert_eq!(
        hash.0,
        ParsedChunk::parse(&chunk)
            .expect("chunk parses")
            .block_hash(0),
        "the local chunk serves the same hash the P2P chunk would",
    );
}

/// A local chunk file whose bytes do not hash to the pinned constant is rejected
/// exactly like a corrupt peer chunk: the prime fails as Unavailable after the
/// attempts, never trusting the wrong bytes.
#[tokio::test]
async fn local_source_rejects_wrong_hash_chunk() {
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
    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));

    let source = SnapshotSource::local_files(dir.path(), peer_set);
    let mut hash_source = CfHashSource::new(spec, source, state);

    let error = hash_source
        .ensure_chunk(0, 3)
        .await
        .expect_err("a wrong-hash local chunk must be rejected");
    assert!(
        matches!(error, ConsumeError::Unavailable { .. }),
        "got {error:?}",
    );
    assert!(
        !hash_source.has_chunk(0),
        "a wrong-hash chunk is never cached",
    );
}

/// The local-file tree source yields the same verified bytes the P2P path would,
/// and `fetch_and_verify_tree` accepts them against the chunk's recorded root.
#[tokio::test]
async fn local_source_tree_matches_p2p() {
    let _init = zebra_test::init();

    let (tree_bytes, chunk_bytes) = sapling_tree_and_chunk();
    let parsed = ParsedChunk::parse(&chunk_bytes).expect("chunk parses");

    let dir = TempDir::new().expect("temp dir is creatable");
    write_tree_records(
        &dir.path().join(SAPLING_TREES_FILE),
        &[(0, tree_bytes.clone())],
    );

    // The peer set must never be asked.
    let peer_set = service_fn(|_req: zn::Request| {
        panic!("the peer set must not be asked when a local tree file exists");
        #[allow(unreachable_code)]
        ready(Ok::<_, BoxError>(zn::Response::NotFound))
    });
    let source = SnapshotSource::local_files(dir.path(), peer_set);

    let verified = fetch_and_verify_tree(&source, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
        .await
        .expect("the local tree verifies against the chunk's recorded root");
    assert_eq!(
        verified, tree_bytes,
        "the local tree yields the same bytes the P2P tree would",
    );
}

/// A local tree file whose frontier does not match the chunk's recorded root is
/// rejected exactly like a corrupt peer tree (the fetch resolves Unavailable).
#[tokio::test]
async fn local_source_rejects_wrong_root_tree() {
    let _init = zebra_test::init();

    // The chunk records a deliberately wrong root, so the real serialized tree's
    // root will not match it.
    let tree = zebra_chain::sapling::tree::NoteCommitmentTree::default();
    let tree_bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("serialization never fails");
    let chunk = build_chunk(
        2,
        &[TreeRoot {
            rel_height: 0,
            root: [0x11u8; 32],
        }],
        &[],
    );
    let parsed = ParsedChunk::parse(&chunk).expect("chunk parses");

    let dir = TempDir::new().expect("temp dir is creatable");
    write_tree_records(&dir.path().join(SAPLING_TREES_FILE), &[(0, tree_bytes)]);

    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));
    let source = SnapshotSource::local_files(dir.path(), peer_set);

    let error = fetch_and_verify_tree(&source, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
        .await
        .expect_err("a local tree whose root disagrees with the chunk must be rejected");
    assert!(
        matches!(error, ConsumeError::Unavailable { .. }),
        "got {error:?}",
    );
}

/// The local-file set source assembles the same bytes the P2P range serve
/// returns, and `fetch_and_verify_set` accepts them against the pinned set hash.
#[tokio::test]
async fn local_source_set_matches_p2p() {
    let _init = zebra_test::init();

    // A set of 8-byte records, the same shape the unspent-output set has.
    let set_bytes: Vec<u8> = (0..300u64).flat_map(|i| i.to_le_bytes()).collect();
    let set_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(&set_bytes)).into_boxed_str());
    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some(set_hash),
        address_balances_hash: None,
    }));

    let dir = TempDir::new().expect("temp dir is creatable");
    std::fs::write(dir.path().join(UNSPENT_OUTPUTS_FILE), &set_bytes)
        .expect("set file is writable");

    let peer_set = service_fn(|_req: zn::Request| {
        panic!("the peer set must not be asked when a local set file exists");
        #[allow(unreachable_code)]
        ready(Ok::<_, BoxError>(zn::Response::NotFound))
    });
    let source = SnapshotSource::local_files(dir.path(), peer_set);

    let verified = fetch_and_verify_set(
        &source,
        spec,
        "unspent-output",
        UNSPENT_OUTPUTS_FILE,
        set_bytes.len() as u64,
        zs::UNSPENT_OUTPUT_RECORD_LEN,
        3,
        |offset, len| zn::Request::UnspentOutputs { offset, len },
    )
    .await
    .expect("the assembled local set verifies against the pinned hash");
    assert_eq!(
        verified, set_bytes,
        "the local set yields the same bytes the P2P set would",
    );
}

/// A local set file whose bytes do not hash to the pinned constant is rejected
/// exactly like a tampered P2P assembly.
#[tokio::test]
async fn local_source_rejects_wrong_hash_set() {
    let _init = zebra_test::init();

    let set_bytes: Vec<u8> = vec![0x42; 120];
    let set_hash: &'static str =
        Box::leak(hex::encode(Sha256::digest(&set_bytes)).into_boxed_str());
    let spec: &'static KnownHashListSpec = Box::leak(Box::new(KnownHashListSpec {
        max_height: Height(0),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "synthetic-known-hashes",
        chunk_hashes: Box::leak(Box::new(["00"])),
        unspent_outputs_hash: Some(set_hash),
        address_balances_hash: None,
    }));

    // Write a byte-flipped set file: assembly succeeds but the hash check fails.
    let mut tampered = set_bytes.clone();
    tampered[0] ^= 0xFF;
    let dir = TempDir::new().expect("temp dir is creatable");
    std::fs::write(dir.path().join(ADDRESS_BALANCES_FILE), &tampered)
        .expect("set file is writable");

    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));
    let source = SnapshotSource::local_files(dir.path(), peer_set);

    let error = fetch_and_verify_set(
        &source,
        spec,
        "unspent-output",
        ADDRESS_BALANCES_FILE,
        set_bytes.len() as u64,
        zs::UNSPENT_OUTPUT_RECORD_LEN,
        3,
        |offset, len| zn::Request::UnspentOutputs { offset, len },
    )
    .await
    .expect_err("a wrong-hash local set must be rejected");
    assert!(
        matches!(error, ConsumeError::SetHashMismatch { .. }),
        "got {error:?}",
    );
}

/// The `LocalSnapshotSource` reader round-trips multi-record tree files and
/// reports a malformed (truncated) file as an error.
#[test]
fn local_snapshot_source_reads_tree_records() {
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
