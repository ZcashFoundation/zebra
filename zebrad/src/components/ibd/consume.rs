//! Snapshot-consume support for the known-hash IBD engine.
//!
//! In snapshot-consume (assumeUTXO) mode the engine does not derive state from
//! the blocks it commits. Instead it downloads, over P2P and content-addressed
//! against pinned SHA-256 constants:
//!
//! - **known-hash chunks** ([`CfHashSource`]): the block hashes, size hints, and
//!   per-height shielded tree roots, read from the `known_hash_chunk` column
//!   family and fetched from peers on a miss
//!   ([`Request::KnownHashChunk`](zn::Request::KnownHashChunk));
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
        KnownHashList, KnownHashListSpec, HASHES_PER_CHUNK,
    },
};
use zebra_network::{self as zn, ShieldedPool};
use zebra_state::{self as zs, note_commitment_tree_root_from_bytes, MAX_SNAPSHOT_RANGE_BYTES};

use crate::{components::ibd::engine::HashSource, BoxError};

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

/// A [`HashSource`] backed by the `known_hash_chunk` column family, with a
/// bundled `.bin` fallback for cold start.
///
/// `hash` and `size_hint` are synchronous (the engine queries them inside its
/// refill step), so they read from the in-memory cache of verified chunks
/// populated by [`ensure_chunk`](Self::ensure_chunk). A cold-start request that
/// has no resident chunk falls back to the bundled list, which the loader
/// already verified against the pinned hashes at open.
///
/// Fetching a missing chunk from a peer is inherently asynchronous, so it is not
/// done from `hash`: the engine's bootstrap calls [`ensure_chunk`](Self::ensure_chunk)
/// for the active window before the heights it covers are needed. A chunk fetched
/// from a peer is verified against the pinned hash before it is cached.
pub struct CfHashSource {
    /// The pinned spec (trust root): max height, per-chunk SHA-256s.
    spec: &'static KnownHashListSpec,

    /// The bundled `.bin` list, used as the cold-start fallback for `hash` /
    /// `size_hint` before any chunk is fetched into the cache.
    ///
    /// `None` when no bundled assets are present: a pure-P2P snapshot-consume
    /// node relies on [`ensure_chunk`](Self::ensure_chunk) priming the CF/RAM
    /// cache before any height is queried, so a cold-start fallback miss is an
    /// engine error rather than a silent gap.
    fallback: Option<KnownHashList>,

    /// Verified, peer-fetched chunks, keyed by chunk index. Each entry's bytes
    /// hashed to the pinned `chunk_hashes[index]` constant before it was
    /// inserted, so they are fully trusted. Re-parsed on lookup (cheap: a bounds
    /// and structure check over a borrowed slice).
    chunks: HashMap<u32, Vec<u8>>,
}

impl CfHashSource {
    /// Returns a new CF-backed hash source over `spec`, with `fallback` as the
    /// bundled cold-start list.
    pub fn new(spec: &'static KnownHashListSpec, fallback: KnownHashList) -> Self {
        Self {
            spec,
            fallback: Some(fallback),
            chunks: HashMap::new(),
        }
    }

    /// Returns a new CF-backed hash source over `spec` with no bundled
    /// cold-start fallback: every height must be served from a chunk primed into
    /// the cache by [`ensure_chunk`](Self::ensure_chunk).
    pub fn without_fallback(spec: &'static KnownHashListSpec) -> Self {
        Self {
            spec,
            fallback: None,
            chunks: HashMap::new(),
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

    /// Ensures the chunk at `index` is resident: returns immediately if it is
    /// already cached or already stored in the `known_hash_chunk` column family,
    /// otherwise fetches it from a peer, verifies it against the pinned hash, and
    /// caches it.
    ///
    /// `attempts` peers are tried before giving up with
    /// [`ConsumeError::Unavailable`]; a chunk that arrives but fails
    /// verification is discarded and the next peer is tried. A verified chunk is
    /// cached in RAM for the synchronous `hash` / `size_hint` lookups, and (when
    /// it came from a peer) is the exact content-addressed bytes that may also be
    /// written back to the CF for re-serving.
    ///
    /// The `state` read service is consulted first so a chunk persisted by a
    /// previous run (or generated by an already-synced state) is reused without a
    /// network round-trip.
    pub async fn ensure_chunk<ZN, ZS>(
        &mut self,
        peer_set: &ZN,
        state: &ZS,
        index: u32,
        attempts: u32,
    ) -> Result<(), ConsumeError>
    where
        ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
        ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone,
    {
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
        // against the pinned hash before it is trusted.
        if let Some(bytes) = self.read_local_chunk(state, index).await? {
            if let Ok(verified) = verify_chunk_bytes(self.spec, index, bytes) {
                self.chunks.insert(index, verified);
                return Ok(());
            }
            // A locally-stored chunk that fails verification is corrupt; fall
            // through to a peer fetch rather than trusting it.
        }

        for _ in 0..attempts {
            let bytes = match self.fetch_chunk_from_peer(peer_set, index).await? {
                Some(bytes) => bytes,
                None => continue,
            };

            match verify_chunk_bytes(self.spec, index, bytes) {
                Ok(verified) => {
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
    /// family via the state read service, or `None` if it is not stored.
    async fn read_local_chunk<ZS>(
        &self,
        state: &ZS,
        index: u32,
    ) -> Result<Option<Vec<u8>>, ConsumeError>
    where
        ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone,
    {
        let response = state
            .clone()
            .oneshot(zs::Request::KnownHashChunk(index))
            .await
            .map_err(ConsumeError::PeerSet)?;

        match response {
            zs::Response::KnownHashChunk(maybe_bytes) => Ok(maybe_bytes),
            _ => Ok(None),
        }
    }

    /// Fetches the chunk at `index` from a single peer, or `None` if the peer
    /// answered `NotFound` (it lacks the chunk).
    async fn fetch_chunk_from_peer<ZN>(
        &self,
        peer_set: &ZN,
        index: u32,
    ) -> Result<Option<Vec<u8>>, ConsumeError>
    where
        ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
    {
        let response = peer_set
            .clone()
            .oneshot(zn::Request::KnownHashChunk(index))
            .await
            .map_err(ConsumeError::PeerSet)?;

        match response {
            zn::Response::KnownHashChunk(bytes) => Ok(Some(bytes.to_vec())),
            zn::Response::NotFound => Ok(None),
            // Any other response is a peer/protocol bug; treat it as a miss so
            // the caller tries another peer.
            _ => Ok(None),
        }
    }
}

impl HashSource for CfHashSource {
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

        // Cold start: fall back to the bundled list, which the loader verified
        // against the pinned hashes at open. With no fallback, the chunk for
        // `height` was not primed: the caller must `ensure_chunk` first.
        match &mut self.fallback {
            Some(fallback) => Ok(KnownHashList::hash(fallback, height)?),
            None => Err(format!(
                "no known-hash chunk is resident for {height:?} and no bundled \
                 fallback is configured; the snapshot-consume bootstrap must \
                 fetch the covering chunk before this height is requested"
            )
            .into()),
        }
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

        // Cold-start fallback: the bundled list's hint, or the conservative
        // default on a chunk read error or with no fallback (the hint only sizes
        // fetch batches, so a default is always safe).
        match &mut self.fallback {
            Some(fallback) => match KnownHashList::size_hint(fallback, height) {
                Ok(Some(hint)) => hint.max(1),
                Ok(None) | Err(_) => chunk_v2::DEFAULT_SIZE_HINT,
            },
            None => chunk_v2::DEFAULT_SIZE_HINT,
        }
    }

    fn release_below(&mut self, height: block::Height) {
        // Drop verified chunks whose whole span is below `height`, and release
        // the bundled fallback's resident chunks too.
        let index = chunk_index_for_height(height);
        self.chunks.retain(|&chunk_index, _| chunk_index >= index);
        if let Some(fallback) = &mut self.fallback {
            KnownHashList::release_below(fallback, height);
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
    for _ in 0..attempts {
        let response = peer_set
            .clone()
            .oneshot(zn::Request::NoteCommitmentTree { pool, height })
            .await
            .map_err(ConsumeError::PeerSet)?;

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

/// Fetches a whole snapshot set in `MAX_SNAPSHOT_RANGE_BYTES` ranges, assembles
/// it, and verifies it against the pinned SHA-256 constant for `set`.
///
/// `total_len` is the set's full byte length (known from the snapshot metadata
/// or discovered by fetching until a short range). `make_request` builds the
/// per-range network request for the set
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
    attempts: u32,
    make_request: F,
) -> Result<Vec<u8>, ConsumeError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone,
    F: Fn(u64, u32) -> zn::Request,
{
    // Fail before any network work if the set cannot be verified at all.
    pinned_set_hash(spec, set)?;

    let mut assembled: Vec<u8> = Vec::with_capacity(total_len as usize);
    let mut offset: u64 = 0;

    while offset < total_len {
        // `MAX_SNAPSHOT_RANGE_BYTES` fits a u32, and the remaining length is at
        // most `total_len`, so this min fits a u32 too.
        let remaining = total_len - offset;
        let len = remaining.min(MAX_SNAPSHOT_RANGE_BYTES) as u32;

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
    for _ in 0..attempts {
        let response = peer_set
            .clone()
            .oneshot(make_request(offset, len))
            .await
            .map_err(ConsumeError::PeerSet)?;

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
