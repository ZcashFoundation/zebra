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

    let verified =
        fetch_and_verify_tree(&peer_set, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
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

    let verified =
        fetch_and_verify_tree(&peer_set, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
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

    let error = fetch_and_verify_tree(&peer_set, ShieldedPool::Sapling, Height(0), 0, &parsed, 3)
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

    let verified = fetch_and_verify_set(
        &peer_set,
        spec,
        "unspent-output",
        set_bytes.len() as u64,
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

    let error = fetch_and_verify_set(
        &peer_set,
        spec,
        "unspent-output",
        set_bytes.len() as u64,
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

/// `ensure_chunk` reads the chunk from the local state CF when it is present,
/// verifies it, and caches it without a network round-trip.
#[tokio::test]
async fn ensure_chunk_uses_local_cf_when_present() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    let mut source = CfHashSource::without_fallback(spec);

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

    source
        .ensure_chunk(&peer_set, &state, 0, 3)
        .await
        .expect("a locally-stored, valid chunk is accepted");
    assert!(
        source.has_chunk(0),
        "the chunk is cached after ensure_chunk"
    );
}

/// `ensure_chunk` fetches from a peer and verifies when the CF lacks the chunk.
#[tokio::test]
async fn ensure_chunk_fetches_and_verifies_from_peer() {
    let _init = zebra_test::init();

    let chunk = build_chunk(4, &[], &[]);
    let spec = leak_spec_for(&chunk, 3);

    let mut source = CfHashSource::without_fallback(spec);

    // The state has no stored chunk; the peer serves the matching one.
    let state = service_fn(|_req: zs::Request| {
        ready(Ok::<_, BoxError>(zs::Response::KnownHashChunk(None)))
    });
    let served = chunk.clone();
    let peer_set = service_fn(move |req: zn::Request| {
        assert!(matches!(req, zn::Request::KnownHashChunk(0)));
        let bytes = served.clone();
        ready(Ok::<_, BoxError>(zn::Response::KnownHashChunk(
            bytes.into(),
        )))
    });

    source
        .ensure_chunk(&peer_set, &state, 0, 3)
        .await
        .expect("a verified peer chunk is accepted");
    assert!(source.has_chunk(0), "the peer chunk is cached");

    // The cached chunk now backs the synchronous hash lookup.
    let hash = HashSource::hash(&mut source, Height(0)).expect("hash lookup succeeds");
    assert!(hash.is_some(), "the cached chunk serves height 0's hash");
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
/// the tree-lookahead source tests, without a network or state round-trip.
async fn primed_source(chunk_bytes: Vec<u8>, max_height: u32) -> CfHashSource {
    let spec = leak_spec_for(&chunk_bytes, max_height);
    let mut source = CfHashSource::without_fallback(spec);

    // Serve the chunk from the local "CF" so `ensure_chunk` caches it.
    let stored = chunk_bytes.clone();
    let state = service_fn(move |_req: zs::Request| {
        let bytes = stored.clone();
        ready(Ok::<_, BoxError>(zs::Response::KnownHashChunk(Some(bytes))))
    });
    let peer_set = service_fn(|_req: zn::Request| ready(Ok::<_, BoxError>(zn::Response::NotFound)));

    source
        .ensure_chunk(&peer_set, &state, 0, 1)
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
