//! Snapshot-consume support for the known-hash IBD engine.
//!
//! In snapshot-consume (assumeUTXO) mode the engine does not derive state from
//! the blocks it commits. Instead it downloads, over P2P and content-addressed
//! against pinned SHA-256 constants:
//!
//! - **known-hash chunks** ([`CfHashSource`]): the block hashes, size hints, and
//!   per-height shielded tree roots, read from the `known_hash_chunk` column
//!   family and fetched from peers on a miss as reassembled byte ranges
//!   ([`Request::KnownHashChunkRange`](zn::Request::KnownHashChunkRange));
//! - **note commitment trees** ([`fetch_and_verify_tree`]): each block's
//!   sapling/orchard tree frontier
//!   ([`Request::NoteCommitmentTree`](zn::Request::NoteCommitmentTree)),
//!   verified against the tree root recorded in the chunk;
//! - **the unspent-output set and the address-balance set**
//!   ([`fetch_and_verify_set`]): ranged byte downloads
//!   ([`Request::UnspentOutputs`](zn::Request::UnspentOutputs) /
//!   [`Request::AddressBalances`](zn::Request::AddressBalances)) assembled and
//!   verified against the pinned set hash.
//!
//! See `docs/design/p2p-snapshot-distribution.md` (architecture and consume
//! model) and `docs/design/utxo-elision.md` (crash-safety of not deriving
//! state). The state-side write of the downloaded snapshot is configured under
//! `[state] snapshot_consume`; this module is the engine-side fetch and
//! verification.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Hash},
    parameters::known_hashes::{
        chunk_v2::{self, ParsedChunk},
        KnownHashListSpec, HASHES_PER_CHUNK,
    },
};
use zebra_network::{self as zn, ShieldedPool};
use zebra_state::{self as zs, note_commitment_tree_root_from_bytes, MAX_SNAPSHOT_RANGE_BYTES};

use crate::{
    components::ibd::{engine::HashSource, SNAPSHOT_FETCH_ATTEMPTS},
    BoxError,
};

/// Errors fetching or verifying snapshot-consume artifacts from peers.
#[derive(Debug, Error)]
pub enum ConsumeError {
    /// A peer's known-hash chunk did not hash to the pinned `chunk_hashes`
    /// constant: a corrupt or adversarial chunk, never trusted.
    #[error(
        "known-hash chunk {index} from a peer hashed to {actual}, \
         but the pinned constant is {expected}; discarding it"
    )]
    ChunkHashMismatch {
        /// The chunk index.
        index: u32,
        /// The pinned SHA-256, as lowercase hex.
        expected: String,
        /// The peer's bytes' SHA-256, as lowercase hex.
        actual: String,
    },

    /// A peer's known-hash chunk did not parse as a valid v2 chunk.
    #[error("known-hash chunk {index} from a peer did not parse as v2: {source}")]
    ChunkParse {
        /// The chunk index.
        index: u32,
        /// The parse failure.
        #[source]
        source: chunk_v2::ChunkV2Error,
    },

    /// The chunk index has no pinned hash in the spec: it is past the end of the
    /// list, so it can never be verified.
    #[error("known-hash chunk index {index} is past the end of the pinned list ({chunks} chunks)")]
    ChunkIndexOutOfRange {
        /// The requested chunk index.
        index: u32,
        /// The number of chunks the pinned spec covers.
        chunks: usize,
    },

    /// No peer served the requested chunk, tree, or snapshot range, after the
    /// allowed attempts.
    #[error("no peer served {artifact} after {attempts} attempts")]
    Unavailable {
        /// A short description of the artifact (`chunk 3`, `sapling tree at 100`).
        artifact: String,
        /// How many peers were asked.
        attempts: u32,
    },

    /// A downloaded note commitment tree's root did not match the root recorded
    /// in the known-hash chunk for its height.
    #[error(
        "{pool:?} note commitment tree at {height:?} from a peer has root {actual}, \
         but the chunk records {expected}; discarding it"
    )]
    TreeRootMismatch {
        /// The shielded pool.
        pool: ShieldedPool,
        /// The block height.
        height: block::Height,
        /// The chunk's recorded root, as lowercase hex.
        expected: String,
        /// The downloaded tree's root, as lowercase hex.
        actual: String,
    },

    /// A downloaded note commitment tree did not deserialize.
    #[error("{pool:?} note commitment tree at {height:?} from a peer did not deserialize")]
    TreeDeserialize {
        /// The shielded pool.
        pool: ShieldedPool,
        /// The block height.
        height: block::Height,
    },

    /// The known-hash chunk has no recorded tree root at or before the height,
    /// so a downloaded tree cannot be verified.
    #[error(
        "no {pool:?} tree root is recorded at or before {height:?} in the known-hash chunk; \
         cannot verify a downloaded tree"
    )]
    NoRecordedTreeRoot {
        /// The shielded pool.
        pool: ShieldedPool,
        /// The block height.
        height: block::Height,
    },

    /// An assembled snapshot set did not hash to its pinned constant.
    #[error("the assembled {set} set hashed to {actual}, but the pinned constant is {expected}")]
    SetHashMismatch {
        /// Which set (`unspent-output` / `address-balance`).
        set: &'static str,
        /// The pinned SHA-256, as lowercase hex.
        expected: String,
        /// The assembled set's SHA-256, as lowercase hex.
        actual: String,
    },

    /// A snapshot set has no pinned hash in the spec, so it cannot be verified
    /// and snapshot-consume sync must not proceed.
    #[error(
        "the {set} set has no pinned SHA-256 constant for this network; \
         snapshot-consume sync cannot verify it (the snapshot updater has not \
         pinned it into source yet)"
    )]
    SetHashNotPinned {
        /// Which set (`unspent-output` / `address-balance`).
        set: &'static str,
    },

    /// A snapshot set's caller-supplied total length is above the sanity cap, so
    /// assembling it would require an unreasonable allocation: refused before any
    /// network work.
    #[error(
        "the {set} set's reported length {total_len} bytes exceeds the {cap}-byte \
         sanity cap; refusing to assemble it"
    )]
    SetTooLarge {
        /// Which set (`unspent-output` / `address-balance`).
        set: &'static str,
        /// The caller-supplied total byte length.
        total_len: u64,
        /// The sanity cap on a snapshot set's byte length.
        cap: u64,
    },

    /// The peer set service failed, before any artifact could be fetched.
    #[error("the peer set service failed: {0}")]
    PeerSet(#[source] BoxError),
}

/// Verifies peer-supplied `bytes` against the pinned chunk hash for `index` and
/// parses them as a v2 chunk.
///
/// This is the content-addressed gate for the CF-backed hash source: a chunk is
/// trusted only after its SHA-256 matches `spec.chunk_hashes[index]` (the trust
/// root reviewed into `zebra-chain`) and it parses as a structurally valid v2
/// chunk. The returned bytes are the verified bytes, suitable for storing into
/// the `known_hash_chunk` column family verbatim.
///
/// Returns the verified bytes on success.
pub fn verify_chunk_bytes(
    spec: &KnownHashListSpec,
    index: u32,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, ConsumeError> {
    let expected = spec.chunk_hashes.get(index as usize).ok_or_else(|| {
        ConsumeError::ChunkIndexOutOfRange {
            index,
            chunks: spec.chunk_hashes.len(),
        }
    })?;

    let actual = hex::encode(Sha256::digest(&bytes));
    if &actual.as_str() != expected {
        return Err(ConsumeError::ChunkHashMismatch {
            index,
            expected: (*expected).to_string(),
            actual,
        });
    }

    // Parse to reject a chunk that hashes correctly but is structurally invalid
    // (impossible for an honest peer, but the parser is the structural gate the
    // rest of the engine relies on). The parse borrows `bytes`, so re-parse at
    // use sites; here it only validates.
    ParsedChunk::parse(&bytes).map_err(|source| ConsumeError::ChunkParse { index, source })?;

    Ok(bytes)
}

/// Verifies a downloaded note commitment tree against a known-hash chunk's
/// recorded root.
///
/// `tree_bytes` is the peer-supplied serialization; `chunk` is the parsed v2
/// chunk covering `height`; `span_base` is the chunk's first absolute height
/// (`chunk_index * HASHES_PER_CHUNK`). The tree is deserialized and its
/// `.root()` is checked against the chunk's `*_root_at_or_before(rel)` for the
/// within-span offset `rel = height - span_base`.
///
/// Returns the verified `tree_bytes` on success, so the caller can hand the
/// exact verified bytes to the state's supplied-tree commit path.
pub fn verify_tree_root(
    pool: ShieldedPool,
    height: block::Height,
    span_base: u32,
    chunk: &ParsedChunk<'_>,
    tree_bytes: Vec<u8>,
) -> Result<Vec<u8>, ConsumeError> {
    // `height >= span_base` for any height the chunk covers; the engine only
    // verifies trees for in-window heights, so this never underflows in
    // practice. Use a checked subtraction to fail safe rather than panic.
    let rel = height
        .0
        .checked_sub(span_base)
        .ok_or(ConsumeError::NoRecordedTreeRoot { pool, height })?;

    let zs_pool = network_to_state_pool(pool);
    let actual = note_commitment_tree_root_from_bytes(zs_pool, &tree_bytes)
        .ok_or(ConsumeError::TreeDeserialize { pool, height })?;

    let expected = match pool {
        ShieldedPool::Sapling => chunk.sapling_root_at_or_before(rel),
        ShieldedPool::Orchard => chunk.orchard_root_at_or_before(rel),
    }
    .ok_or(ConsumeError::NoRecordedTreeRoot { pool, height })?;

    if actual != expected {
        return Err(ConsumeError::TreeRootMismatch {
            pool,
            height,
            expected: hex::encode(expected),
            actual: hex::encode(actual),
        });
    }

    Ok(tree_bytes)
}

/// Maps the network shielded-pool selector onto the state shielded-pool
/// selector. The two enums are kept separate so `zebra-state` does not depend on
/// the higher `zebra-network` crate.
fn network_to_state_pool(pool: ShieldedPool) -> zs::ShieldedPool {
    match pool {
        ShieldedPool::Sapling => zs::ShieldedPool::Sapling,
        ShieldedPool::Orchard => zs::ShieldedPool::Orchard,
    }
}

/// The pinned SHA-256, as lowercase hex, of a downloadable snapshot set, or an
/// error if the set has no pinned constant for this network yet.
fn pinned_set_hash<'a>(
    spec: &'a KnownHashListSpec,
    set: &'static str,
) -> Result<&'a str, ConsumeError> {
    let pinned = match set {
        "unspent-output" => spec.unspent_outputs_hash,
        "address-balance" => spec.address_balances_hash,
        other => unreachable!("unknown snapshot set name {other}"),
    };
    pinned.ok_or(ConsumeError::SetHashNotPinned { set })
}

/// Verifies an assembled snapshot `set` against its pinned SHA-256 constant.
///
/// Returns the verified `bytes` on success, so the caller hands the exact
/// verified bytes to the state (the survivor filter or the balance loader).
pub fn verify_set_hash(
    spec: &KnownHashListSpec,
    set: &'static str,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, ConsumeError> {
    let expected = pinned_set_hash(spec, set)?;

    let actual = hex::encode(Sha256::digest(&bytes));
    if actual.as_str() != expected {
        return Err(ConsumeError::SetHashMismatch {
            set,
            expected: expected.to_string(),
            actual,
        });
    }

    Ok(bytes)
}

/// The absolute first height of the chunk at `index`: `index * HASHES_PER_CHUNK`.
///
/// Returns `None` if the multiply overflows (a chunk index far past any real
/// height), so callers fail safe rather than wrap.
pub fn chunk_span_base(index: u32) -> Option<u32> {
    index.checked_mul(HASHES_PER_CHUNK)
}

/// The chunk index covering `height`: `height / HASHES_PER_CHUNK`.
pub fn chunk_index_for_height(height: block::Height) -> u32 {
    // `HASHES_PER_CHUNK` is a non-zero constant, so the division never panics.
    height.0 / HASHES_PER_CHUNK
}

/// A [`HashSource`] backed by the `known_hash_chunk` column family.
///
/// `hash` and `size_hint` are synchronous (the engine queries them inside its
/// refill step), so they read from the in-memory cache of verified chunks
/// populated by [`ensure_chunk`](Self::ensure_chunk). There is no bundled
/// `.bin` fallback: the v1 `.bin` list carries no per-height tree roots, so it
/// cannot be reconciled with the v2 chunks the cache stores, and a v1/v2
/// disagreement at a chunk boundary would be invisible to the synchronous
/// lookups. Instead every height is served from a v2 chunk that
/// [`ensure_covers`](Self::ensure_covers) has primed (fetched-and-verified, or
/// read back from the CF) before the height is queried; a height with no
/// resident covering chunk is an engine error, surfaced as a retryable failure,
/// rather than a silently divergent fallback.
///
/// Fetching a missing chunk from a peer is inherently asynchronous, so it is not
/// done from `hash`: the engine primes the active window via
/// [`ensure_covers`](Self::ensure_covers) — at bootstrap and again as the commit
/// frontier advances across chunk boundaries — before the heights it covers are
/// needed. A chunk fetched from a peer is verified against the pinned hash, then
/// persisted to the CF (so a restart does not re-fetch it) before it is cached.
///
/// Holds clones of the network and state services so it is self-sufficient for
/// fetching, reading the CF, and persisting verified chunks.
pub struct CfHashSource<ZN, ZS> {
    /// The pinned spec (trust root): max height, per-chunk SHA-256s.
    spec: &'static KnownHashListSpec,

    /// Verified chunks, keyed by chunk index. Each entry's bytes hashed to the
    /// pinned `chunk_hashes[index]` constant before it was inserted, so they are
    /// fully trusted. Re-parsed on lookup (cheap: a bounds and structure check
    /// over a borrowed slice).
    chunks: HashMap<u32, Vec<u8>>,

    /// The network service, used to fetch chunks ranged from peers.
    peer_set: ZN,

    /// The state service, used to read chunks from the local
    /// `known_hash_chunk` column family and to persist verified chunks back into
    /// it.
    state: ZS,
}

impl<ZN, ZS> CfHashSource<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone,
    ZS::Future: Send,
{
    /// Returns a new CF-backed hash source over `spec`, fetching chunks through
    /// `peer_set` and reading/persisting them through `state`.
    pub fn new(spec: &'static KnownHashListSpec, peer_set: ZN, state: ZS) -> Self {
        Self {
            spec,
            chunks: HashMap::new(),
            peer_set,
            state,
        }
    }

    /// The pinned spec backing this source.
    pub fn spec(&self) -> &'static KnownHashListSpec {
        self.spec
    }

    /// Returns the verified, parsed chunk covering `height`, if it is resident
    /// in the cache.
    ///
    /// Used by the tree-fetch path to look up the recorded tree root for a
    /// height. The chunk is re-parsed from its verified bytes on each call.
    pub fn parsed_chunk_for_height(&self, height: block::Height) -> Option<(u32, ParsedChunk<'_>)> {
        let index = chunk_index_for_height(height);
        let span_base = chunk_span_base(index)?;
        let bytes = self.chunks.get(&index)?;
        // The bytes were verified before insertion, so the parse never fails;
        // returning `None` on a parse error fails safe regardless.
        let parsed = ParsedChunk::parse(bytes).ok()?;
        Some((span_base, parsed))
    }

    /// Whether the chunk at `index` is resident in the verified cache.
    pub fn has_chunk(&self, index: u32) -> bool {
        self.chunks.contains_key(&index)
    }

    /// Ensures every chunk covering `[start, end]` (inclusive heights) is
    /// resident, priming each missing one via [`ensure_chunk`](Self::ensure_chunk).
    ///
    /// Called at bootstrap for the first window and again as the commit frontier
    /// advances, so the synchronous [`hash`](HashSource::hash) /
    /// [`size_hint`](HashSource::size_hint) / tree-root lookups always find a
    /// resident chunk — including after the frontier crosses a chunk boundary
    /// (every `HASHES_PER_CHUNK` heights). A chunk index past the end of the
    /// pinned list is skipped (there is nothing to fetch there); any other
    /// failure propagates so the supervisor can restart the bootstrap.
    pub async fn ensure_covers(
        &mut self,
        start: block::Height,
        end: block::Height,
        attempts: u32,
    ) -> Result<(), ConsumeError> {
        let first_index = chunk_index_for_height(start);
        let last_index = chunk_index_for_height(end);
        // `chunk_hashes.len() >= 1` for a valid spec; the count is far below
        // `u32::MAX`, so this cast never truncates.
        let max_index = self.spec.chunk_hashes.len().saturating_sub(1) as u32;

        for index in first_index..=last_index.min(max_index) {
            self.ensure_chunk(index, attempts).await?;
        }

        Ok(())
    }

    /// Ensures the chunk at `index` is resident: returns immediately if it is
    /// already cached, otherwise reuses a chunk already stored in the
    /// `known_hash_chunk` column family, or fetches it from a peer, verifies it
    /// against the pinned hash, persists it to the CF, and caches it.
    ///
    /// `attempts` peers are tried before giving up with
    /// [`ConsumeError::Unavailable`]; a chunk that arrives but fails
    /// verification is discarded and the next peer is tried, and a transient
    /// peer-set or state-service failure on one attempt is tolerated — it is
    /// logged and the next peer is tried — so one flaky peer does not abort the
    /// whole prime. A verified peer chunk is the exact content-addressed bytes,
    /// persisted to the CF so a restart (or a peer requesting the chunk) reads it
    /// back without a network round-trip.
    ///
    /// The state CF is consulted first so a chunk persisted by a previous run (or
    /// present because this node is partly synced) is reused without any network
    /// work.
    pub async fn ensure_chunk(&mut self, index: u32, attempts: u32) -> Result<(), ConsumeError> {
        if self.chunks.contains_key(&index) {
            return Ok(());
        }

        // The pinned hash must exist, or the index is past the list end.
        if self.spec.chunk_hashes.get(index as usize).is_none() {
            return Err(ConsumeError::ChunkIndexOutOfRange {
                index,
                chunks: self.spec.chunk_hashes.len(),
            });
        }

        // Reuse a chunk already in the local CF (persisted by a previous run, or
        // present because this node is partly synced). It is still verified
        // against the pinned hash before it is trusted. A transient state-service
        // error here is not fatal: fall through to a peer fetch.
        match self.read_local_chunk(index).await {
            Ok(Some(bytes)) => {
                if let Ok(verified) = verify_chunk_bytes(self.spec, index, bytes) {
                    self.chunks.insert(index, verified);
                    return Ok(());
                }
                // A locally-stored chunk that fails verification is corrupt; fall
                // through to a peer fetch rather than trusting it.
            }
            Ok(None) => {}
            Err(error) => {
                debug!(
                    %error,
                    index,
                    "reading a known-hash chunk from the local state failed; fetching from peers",
                );
            }
        }

        for attempt in 0..attempts {
            // A transient peer-set failure on this attempt is tolerated: log it
            // and try another peer, rather than aborting the whole prime.
            let bytes = match self.fetch_chunk_from_peer(index).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => {
                    debug!(
                        %error,
                        index,
                        attempt,
                        "a peer chunk fetch failed transiently; trying another peer",
                    );
                    continue;
                }
            };

            match verify_chunk_bytes(self.spec, index, bytes) {
                Ok(verified) => {
                    // Persist the verified bytes so a restart (or a peer
                    // requesting this chunk) reads them back without re-fetching.
                    // A persist failure is not fatal — the chunk is already
                    // trusted in RAM — so it is logged and the prime succeeds.
                    if let Err(error) = self.persist_chunk(index, &verified).await {
                        warn!(
                            %error,
                            index,
                            "failed to persist a verified known-hash chunk to the state; \
                             it is cached in RAM but a restart will re-fetch it",
                        );
                    }
                    self.chunks.insert(index, verified);
                    return Ok(());
                }
                // A corrupt/adversarial chunk: discard and try another peer.
                Err(ConsumeError::ChunkHashMismatch { .. })
                | Err(ConsumeError::ChunkParse { .. }) => continue,
                Err(other) => return Err(other),
            }
        }

        Err(ConsumeError::Unavailable {
            artifact: format!("chunk {index}"),
            attempts,
        })
    }

    /// Reads the chunk at `index` from the local `known_hash_chunk` column
    /// family via the state service, or `None` if it is not stored.
    async fn read_local_chunk(&self, index: u32) -> Result<Option<Vec<u8>>, ConsumeError> {
        let response = self
            .state
            .clone()
            .oneshot(zs::Request::KnownHashChunk(index))
            .await
            .map_err(ConsumeError::PeerSet)?;

        match response {
            zs::Response::KnownHashChunk(maybe_bytes) => Ok(maybe_bytes),
            _ => Ok(None),
        }
    }

    /// Persists the verified chunk `bytes` at `index` into the local
    /// `known_hash_chunk` column family via the state service.
    ///
    /// The caller verifies `bytes` against the pinned hash before calling this,
    /// so the state stores them content-addressed.
    async fn persist_chunk(&self, index: u32, bytes: &[u8]) -> Result<(), ConsumeError> {
        self.state
            .clone()
            .oneshot(zs::Request::WriteKnownHashChunk {
                index,
                bytes: bytes.to_vec(),
            })
            .await
            .map_err(ConsumeError::PeerSet)?;

        Ok(())
    }

    /// Fetches the whole chunk at `index` from peers by reassembling its
    /// `MAX_SNAPSHOT_RANGE_BYTES` ranges, or `None` if a range could not be
    /// served (so the caller tries the next attempt).
    ///
    /// A full v2 chunk (~4.72 MiB) exceeds `MAX_PROTOCOL_MESSAGE_LEN`, so it is
    /// transferred ranged like the snapshot sets. Unlike the snapshot sets the
    /// total chunk length is not known a priori, so this fetches ascending
    /// ranges until a peer returns a short (or empty) range, marking the chunk's
    /// end; the caller verifies the reassembled bytes' SHA-256 against the pinned
    /// `chunk_hashes[index]` constant, which catches any disagreement on length
    /// or content between peers, so a single adversarial range cannot corrupt the
    /// chunk undetected.
    ///
    /// Bounded: each range is `≤ MAX_SNAPSHOT_RANGE_BYTES`, and the reassembled
    /// chunk is bounded by the maximum v2 chunk size (a function of
    /// `HASHES_PER_CHUNK`), so a peer cannot grow the buffer without bound. A
    /// transient peer-set failure returns `Err`, which the caller tolerates as a
    /// single failed attempt.
    async fn fetch_chunk_from_peer(&self, index: u32) -> Result<Option<Vec<u8>>, ConsumeError> {
        let mut assembled: Vec<u8> = Vec::new();
        let mut offset: u64 = 0;

        loop {
            // The reassembled chunk can never exceed the maximum v2 chunk size; a
            // peer that keeps returning full ranges past that bound is feeding us
            // junk, so stop and let the caller try another attempt. The SHA-256
            // check would reject it anyway; this just bounds the memory first.
            if assembled.len() as u64 > MAX_V2_CHUNK_BYTES {
                return Ok(None);
            }

            // `MAX_SNAPSHOT_RANGE_BYTES` fits a u32 (it is 1 MiB), so the cast
            // never truncates.
            let len = MAX_SNAPSHOT_RANGE_BYTES as u32;

            let response = self
                .peer_set
                .clone()
                .oneshot(zn::Request::KnownHashChunkRange { index, offset, len })
                .await
                .map_err(ConsumeError::PeerSet)?;

            let range = match response {
                zn::Response::SnapshotRange(bytes) => bytes,
                // The peer can't serve this range (unknown/above-tip chunk, or an
                // offset past its chunk end): treat the whole fetch as a miss so
                // the caller tries another peer/attempt.
                zn::Response::NotFound => return Ok(None),
                // Any other response is a peer/protocol bug; treat it as a miss.
                _ => return Ok(None),
            };

            // A short or empty range marks the chunk's end (the peer has no more
            // bytes at this offset). Stop and return what we assembled; the
            // caller's SHA-256 check validates the full chunk.
            let is_final = (range.len() as u64) < u64::from(len);
            offset += range.len() as u64;
            assembled.extend_from_slice(&range);

            if is_final {
                return Ok(Some(assembled));
            }
        }
    }
}

/// The maximum serialized size of a v2 known-hash chunk, used to bound chunk
/// reassembly from untrusted peer ranges.
///
/// A full chunk holds `HASHES_PER_CHUNK` blocks. The v2 layout is the 16-byte
/// header, `n × 32` hashes, `n × 1` size hints, then two tree-root sections.
/// Each tree section has at most one record per height (`4 + 32` bytes), giving a
/// generous upper bound of `16 + n × (32 + 1) + 2 × (4 + n × 36)`. Rounded up to
/// a clean ceiling so an honest chunk always fits and a malicious peer cannot
/// grow the buffer past it.
const MAX_V2_CHUNK_BYTES: u64 = 16
    + HASHES_PER_CHUNK as u64 * (HASH_BYTES_U64 + 1)
    + 2 * (4 + HASHES_PER_CHUNK as u64 * (4 + HASH_BYTES_U64));

/// The byte length of a single 32-byte hash, as a `u64`, for the
/// [`MAX_V2_CHUNK_BYTES`] bound.
const HASH_BYTES_U64: u64 = 32;

/// The sanity cap on a downloadable snapshot set's total byte length, used to
/// bound the up-front assembly buffer in [`fetch_and_verify_set`] against a
/// caller-supplied `total_len`.
///
/// The unspent-output and address-balance sets at `H_max` are at most a few
/// hundred MiB on Mainnet today; 8 GiB is far above any plausible real set
/// (leaving headroom for chain growth) while still refusing an adversarial
/// length that would otherwise drive a huge reservation. The assembled bytes are
/// content-addressed against the pinned hash regardless, so this only guards the
/// allocation, not correctness.
const MAX_SNAPSHOT_SET_BYTES: u64 = 8 * 1024 * 1024 * 1024;

impl<ZN, ZS> HashSource for CfHashSource<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    fn max_height(&self) -> block::Height {
        self.spec.max_height
    }

    fn hash(&mut self, height: block::Height) -> Result<Option<Hash>, BoxError> {
        let index = chunk_index_for_height(height);

        if let Some(bytes) = self.chunks.get(&index) {
            let Some(span_base) = chunk_span_base(index) else {
                return Ok(None);
            };
            let parsed = ParsedChunk::parse(bytes).map_err(|error| Box::new(error) as BoxError)?;
            // `height >= span_base` for an in-range chunk; if the height is past
            // the chunk's block count it is outside the list.
            let rel = height.0 - span_base;
            if rel >= parsed.block_count() {
                return Ok(None);
            }
            let hash_bytes: [u8; 32] = parsed.block_hash(rel);
            return Ok(Some(Hash(hash_bytes)));
        }

        // The covering chunk was not primed. There is no v1 `.bin` fallback (it
        // carries no tree roots and could silently disagree with the v2 chunks):
        // every height is served from a v2 chunk that `ensure_covers` primed, so
        // a miss here is an engine error the supervisor retries, not a divergent
        // fallback.
        Err(format!(
            "no known-hash chunk is resident for {height:?}; the snapshot-consume \
             engine must `ensure_covers` the height before it is requested"
        )
        .into())
    }

    fn size_hint(&mut self, height: block::Height) -> u8 {
        let index = chunk_index_for_height(height);

        if let Some(bytes) = self.chunks.get(&index) {
            if let (Some(span_base), Ok(parsed)) =
                (chunk_span_base(index), ParsedChunk::parse(bytes))
            {
                let rel = height.0.wrapping_sub(span_base);
                if rel < parsed.block_count() {
                    return parsed.size_hint(rel).max(1);
                }
            }
        }

        // No resident chunk for this height (it has not been primed yet): the
        // conservative default. The hint only sizes fetch batches, so a default
        // is always safe; the `hash` lookup for the same height surfaces the
        // missing-chunk error.
        chunk_v2::DEFAULT_SIZE_HINT
    }

    fn release_below(&mut self, height: block::Height) {
        // Drop verified chunks whose whole span is below `height`.
        let index = chunk_index_for_height(height);
        self.chunks.retain(|&chunk_index, _| chunk_index >= index);
    }

    fn ensure_covers(
        &mut self,
        start: block::Height,
        end: block::Height,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + '_>> {
        Box::pin(async move {
            CfHashSource::ensure_covers(self, start, end, SNAPSHOT_FETCH_ATTEMPTS)
                .await
                .map_err(|error| Box::new(error) as BoxError)
        })
    }

    fn tree_updates_in(
        &mut self,
        start: block::Height,
        end: block::Height,
    ) -> Vec<crate::components::ibd::tree::TreeFetch> {
        // Collect the updating heights from every resident chunk that overlaps
        // `[start, end]`. A chunk that is not yet resident contributes nothing:
        // the lookahead simply schedules nothing for its heights until it is
        // primed (the bootstrap primes the active window's chunks first), which
        // is safe — a missing tree just falls back to folding.
        let first_index = chunk_index_for_height(start);
        let last_index = chunk_index_for_height(end);

        let mut updates = Vec::new();

        for index in first_index..=last_index {
            let Some(span_base) = chunk_span_base(index) else {
                continue;
            };
            let Some(bytes) = self.chunks.get(&index) else {
                continue;
            };
            // The bytes were verified before insertion, so the parse never
            // fails; skipping on a parse error fails safe.
            let Ok(parsed) = ParsedChunk::parse(bytes) else {
                continue;
            };

            push_pool_updates(
                &mut updates,
                ShieldedPool::Sapling,
                span_base,
                start,
                end,
                parsed.sapling_roots(),
            );
            push_pool_updates(
                &mut updates,
                ShieldedPool::Orchard,
                span_base,
                start,
                end,
                parsed.orchard_roots(),
            );
        }

        // The lookahead scheduler expects ascending `(height, pool)` order.
        updates.sort_unstable();
        updates
    }

    fn tree_root(&mut self, pool: ShieldedPool, height: block::Height) -> Option<[u8; 32]> {
        let index = chunk_index_for_height(height);
        let span_base = chunk_span_base(index)?;
        let bytes = self.chunks.get(&index)?;
        let parsed = ParsedChunk::parse(bytes).ok()?;

        let rel = height.0.checked_sub(span_base)?;

        // `tree_updates_in` only reports heights that update a pool's tree, so an
        // exactly-recorded root exists for any `(pool, height)` the lookahead
        // asks about. Confirm the recorded record is at this exact rel-height
        // (not merely at-or-before it), so a non-updating height returns `None`
        // rather than a stale earlier root.
        let roots = match pool {
            ShieldedPool::Sapling => parsed.sapling_roots(),
            ShieldedPool::Orchard => parsed.orchard_roots(),
        };
        roots
            .into_iter()
            .find(|record| record.rel_height == rel)
            .map(|record| record.root)
    }
}

/// Appends the absolute updating heights for `pool` that fall inside the
/// inclusive `[start, end]` window into `updates`.
///
/// `roots` are the chunk's sparse `(rel_height, root)` records (ascending);
/// `span_base` is the chunk's first absolute height, so the absolute height is
/// `span_base + rel_height`.
fn push_pool_updates(
    updates: &mut Vec<crate::components::ibd::tree::TreeFetch>,
    pool: ShieldedPool,
    span_base: u32,
    start: block::Height,
    end: block::Height,
    roots: Vec<chunk_v2::TreeRoot>,
) {
    use crate::components::ibd::tree::TreeFetch;

    for record in roots {
        // `span_base + rel_height` cannot overflow a real height (the chunk's
        // span is far below u32::MAX); saturate to fail safe regardless.
        let height = block::Height(span_base.saturating_add(record.rel_height));
        if height >= start && height <= end {
            updates.push(TreeFetch { height, pool });
        }
    }
}

/// Fetches the `pool` note commitment tree as of `height` from a peer and
/// verifies its root against `chunk`'s recorded root.
///
/// `span_base` is the chunk's first absolute height. Tries `attempts` peers; a
/// tree that deserializes but whose root does not match, or that does not
/// deserialize, is discarded and the next peer is tried (an honest peer's tree
/// always verifies). Returns the verified tree bytes, ready to hand to the
/// state's supplied-tree commit path.
pub async fn fetch_and_verify_tree<ZN>(
    peer_set: &ZN,
    pool: ShieldedPool,
    height: block::Height,
    span_base: u32,
    chunk: &ParsedChunk<'_>,
    attempts: u32,
) -> Result<Vec<u8>, ConsumeError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
{
    for attempt in 0..attempts {
        // A transient peer-set failure on this attempt is tolerated: log it and
        // try another peer, rather than aborting the whole fetch.
        let response = match peer_set
            .clone()
            .oneshot(zn::Request::NoteCommitmentTree { pool, height })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                debug!(
                    %error,
                    ?pool,
                    ?height,
                    attempt,
                    "a peer tree fetch failed transiently; trying another peer",
                );
                continue;
            }
        };

        let bytes = match response {
            zn::Response::NoteCommitmentTree(bytes) => bytes.to_vec(),
            // The peer lacks the tree, or answered unexpectedly: try another.
            zn::Response::NotFound => continue,
            _ => continue,
        };

        match verify_tree_root(pool, height, span_base, chunk, bytes) {
            Ok(verified) => return Ok(verified),
            // A corrupt tree: try another peer. A missing recorded root is a
            // hard error (the chunk itself is wrong), so propagate it.
            Err(ConsumeError::TreeRootMismatch { .. })
            | Err(ConsumeError::TreeDeserialize { .. }) => continue,
            Err(other) => return Err(other),
        }
    }

    Err(ConsumeError::Unavailable {
        artifact: format!("{pool:?} tree at {height:?}"),
        attempts,
    })
}

/// Fetches a whole snapshot set in record-aligned ranges, assembles it, and
/// verifies it against the pinned SHA-256 constant for `set`.
///
/// `total_len` is the set's full byte length (known from the snapshot metadata
/// or discovered by fetching until a short range). `record_len` is the set's
/// fixed record size (8 bytes for the unspent-output set, 45 bytes for the
/// address-balance set); ranges are aligned to it so the serve path — which
/// requires record-aligned `offset`/`len`
/// ([`zebra_state::unspent_outputs_range`] /
/// [`zebra_state::address_balances_range`]) — accepts every request.
/// `make_request` builds the per-range network request
/// ([`Request::UnspentOutputs`](zn::Request::UnspentOutputs) or
/// [`Request::AddressBalances`](zn::Request::AddressBalances)). Returns the
/// verified set bytes.
///
/// Each range is `≤ MAX_SNAPSHOT_RANGE_BYTES` to stay well under the 2 MiB
/// protocol frame; the assembled set is content-addressed, so a single
/// adversarial range cannot corrupt it undetected.
pub async fn fetch_and_verify_set<ZN, F>(
    peer_set: &ZN,
    spec: &KnownHashListSpec,
    set: &'static str,
    total_len: u64,
    record_len: u64,
    attempts: u32,
    make_request: F,
) -> Result<Vec<u8>, ConsumeError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
    F: Fn(u64, u32) -> zn::Request,
{
    // Fail before any network work if the set cannot be verified at all.
    pinned_set_hash(spec, set)?;

    // `total_len` is caller-supplied (it comes from snapshot metadata, not a
    // pinned constant), so an over-large value must not drive an unbounded
    // up-front allocation. A set larger than the sanity cap can never be a real
    // snapshot, so refuse it before allocating; the assembled bytes are
    // content-addressed afterwards either way.
    if total_len > MAX_SNAPSHOT_SET_BYTES {
        return Err(ConsumeError::SetTooLarge {
            set,
            total_len,
            cap: MAX_SNAPSHOT_SET_BYTES,
        });
    }

    // The per-range length must be a multiple of `record_len`, since the serve
    // path rejects misaligned `offset`/`len`. Floor `MAX_SNAPSHOT_RANGE_BYTES`
    // to the largest record-aligned length that still fits the frame; the offset
    // stays record-aligned because every range is record-aligned and `total_len`
    // is a multiple of `record_len`.
    let record_len = record_len.max(1);
    let aligned_range = (MAX_SNAPSHOT_RANGE_BYTES / record_len * record_len).max(record_len);

    // `total_len` is bounded by `MAX_SNAPSHOT_SET_BYTES` (checked above), which
    // fits a `usize` on all supported platforms, so the cast never truncates.
    // The Vec still grows naturally if a peer over-delivers; the cap only bounds
    // the up-front reservation.
    let mut assembled: Vec<u8> = Vec::with_capacity(total_len as usize);
    let mut offset: u64 = 0;

    while offset < total_len {
        // `aligned_range` is `<= MAX_SNAPSHOT_RANGE_BYTES` (1 MiB), and the
        // remaining length is at most `total_len`, so this min fits a u32.
        let remaining = total_len - offset;
        let len = remaining.min(aligned_range) as u32;

        let range = fetch_range(peer_set, &make_request, offset, len, set, attempts).await?;

        // A peer that returns a short range (fewer bytes than requested) ends the
        // set early; the final assembled hash check catches any disagreement.
        if range.is_empty() {
            break;
        }
        offset += range.len() as u64;
        assembled.extend_from_slice(&range);
    }

    verify_set_hash(spec, set, assembled)
}

/// Fetches a single snapshot range `[offset, offset + len)` from a peer, trying
/// `attempts` peers.
async fn fetch_range<ZN, F>(
    peer_set: &ZN,
    make_request: &F,
    offset: u64,
    len: u32,
    set: &'static str,
    attempts: u32,
) -> Result<Vec<u8>, ConsumeError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
    F: Fn(u64, u32) -> zn::Request,
{
    for attempt in 0..attempts {
        // A transient peer-set failure on this attempt is tolerated: log it and
        // try another peer, rather than aborting the whole set assembly.
        let response = match peer_set.clone().oneshot(make_request(offset, len)).await {
            Ok(response) => response,
            Err(error) => {
                debug!(
                    %error,
                    set,
                    offset,
                    attempt,
                    "a peer snapshot-range fetch failed transiently; trying another peer",
                );
                continue;
            }
        };

        match response {
            zn::Response::SnapshotRange(bytes) => return Ok(bytes.to_vec()),
            // The peer can't serve this range; try another.
            zn::Response::NotFound => continue,
            _ => continue,
        }
    }

    Err(ConsumeError::Unavailable {
        artifact: format!("{set} range at offset {offset}"),
        attempts,
    })
}

#[cfg(test)]
mod tests;
