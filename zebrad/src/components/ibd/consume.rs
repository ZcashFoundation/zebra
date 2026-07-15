//! Snapshot-consume support for the known-hash IBD engine.
//!
//! In snapshot-consume (assumeUTXO) mode the engine does not derive state from
//! the blocks it commits. Instead it reads the release snapshot artifacts —
//! downloaded by the installer, or emitted locally by
//! `emit-snapshot --emit-files` — from a local directory, content-addressed
//! against pinned SHA-256 constants:
//!
//! - **known-hash chunks** ([`CfHashSource`]): the block hashes, size hints, and
//!   per-height shielded tree roots, read from the `known_hash_chunk` column
//!   family and read from the artifact directory on a miss, verified against
//!   the pinned `chunk_hashes` constants;
//! - **note commitment trees**: the engine's tree-fetch stage reads each
//!   selected height's sapling/orchard frontier from the artifact directory and
//!   verifies it against the tree root recorded in the chunk;
//! - **the unspent-output (survivor) set**: loaded by the state at open time
//!   (`[state] snapshot_consume`). The address-balance and chain-value-pool
//!   bulk loads are not wired up yet (design doc
//!   `snapshot-distribution.md` §8.2), and the state-side set loads do not yet
//!   verify the pinned set hashes — both land with the installer flow.
//!
//! See `docs/design/snapshot-distribution.md` (architecture and consume
//! model) and `docs/design/utxo-elision.md` (crash-safety of not deriving
//! state). The state-side write of the downloaded snapshot is configured under
//! `[state] snapshot_consume`; this module is the engine-side read and
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
use zebra_state::{self as zs, ShieldedPool};

use crate::{components::ibd::engine::HashSource, BoxError};

pub mod local;

pub use local::{LocalSnapshotSource, LocalSourceError};

/// Errors reading or verifying snapshot-consume artifacts.
#[derive(Debug, Error)]
pub enum ConsumeError {
    /// A known-hash chunk did not hash to the pinned `chunk_hashes` constant: a
    /// corrupt or tampered chunk, never trusted.
    #[error(
        "known-hash chunk {index} from the snapshot source hashed to {actual}, \
         but the pinned constant is {expected}; discarding it"
    )]
    ChunkHashMismatch {
        /// The chunk index.
        index: u32,
        /// The pinned SHA-256, as lowercase hex.
        expected: String,
        /// The source's bytes' SHA-256, as lowercase hex.
        actual: String,
    },

    /// A known-hash chunk did not parse as a valid v2 chunk.
    #[error("known-hash chunk {index} from the snapshot source did not parse as v2: {source}")]
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

    /// The state service failed while reading or persisting a chunk.
    #[error("the state service failed: {0}")]
    StateService(#[source] BoxError),

    /// A blocking artifact-read task failed to run to completion (Zebra is
    /// shutting down, or the read panicked).
    #[error("a blocking artifact-read task failed: {0}")]
    ReadTask(#[source] tokio::task::JoinError),

    /// Reading an artifact from the local snapshot source failed.
    #[error("reading an artifact from the local snapshot source failed: {0}")]
    LocalSource(#[source] LocalSourceError),
}

impl ConsumeError {
    /// Whether this error is deterministic: retrying against the same
    /// read-only artifact directory re-reads the same bytes and fails the same
    /// way, so the supervisor should surface it to the operator instead of
    /// restarting.
    ///
    /// Only a state-service or blocking-task failure can be transient (a
    /// shutdown race or an overloaded service); every artifact-content error
    /// (missing/oversized/unreadable file, hash mismatch, parse failure,
    /// out-of-range index) is a property of the configured directory.
    pub fn is_deterministic(&self) -> bool {
        !matches!(
            self,
            ConsumeError::StateService(_) | ConsumeError::ReadTask(_)
        )
    }
}

/// Verifies source-supplied `bytes` against the pinned chunk hash for `index` and
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
    // (impossible for an honest artifact, but the parser is the structural gate the
    // rest of the engine relies on). The parse borrows `bytes`, so re-parse at
    // use sites; here it only validates.
    ParsedChunk::parse(&bytes).map_err(|source| ConsumeError::ChunkParse { index, source })?;

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
/// [`ensure_covers`](Self::ensure_covers) has primed (read-and-verified, or
/// read back from the CF) before the height is queried; a height with no
/// resident covering chunk is an engine error, surfaced as a retryable failure,
/// rather than a silently divergent fallback.
///
/// Reading a missing chunk from the artifact directory goes through the async
/// state service for CF reads, so it is not done from `hash`: the engine primes
/// the active window via [`ensure_covers`](Self::ensure_covers) — at bootstrap
/// and again as the commit frontier advances across chunk boundaries — before
/// the heights it covers are needed. A chunk read from the artifact directory is
/// verified against the pinned hash, then persisted to the CF (so a restart does
/// not re-read it) before it is cached.
///
/// Holds the artifact source and a clone of the state service so it is
/// self-sufficient for reading artifacts, reading the CF, and persisting
/// verified chunks.
pub struct CfHashSource<ZS> {
    /// The pinned spec (trust root): max height, per-chunk SHA-256s.
    spec: &'static KnownHashListSpec,

    /// Verified chunks, keyed by chunk index. Each entry's bytes hashed to the
    /// pinned `chunk_hashes[index]` constant before it was inserted, so they are
    /// fully trusted. Re-parsed on lookup (cheap: a bounds and structure check
    /// over a borrowed slice).
    chunks: HashMap<u32, Vec<u8>>,

    /// The artifact source: the local directory the installer downloaded (or
    /// `emit-snapshot --emit-files` wrote).
    source: LocalSnapshotSource,

    /// The state service, used to read chunks from the local
    /// `known_hash_chunk` column family and to persist verified chunks back into
    /// it.
    state: ZS,
}

impl<ZS> CfHashSource<ZS>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone,
    ZS::Future: Send,
{
    /// Returns a new CF-backed hash source over `spec`, reading chunks through
    /// `source` (the local artifact directory) and reading/persisting them
    /// through `state`.
    pub fn new(spec: &'static KnownHashListSpec, source: LocalSnapshotSource, state: ZS) -> Self {
        Self {
            spec,
            chunks: HashMap::new(),
            source,
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
    /// pinned list is skipped (there is nothing to read there); any other
    /// failure propagates to the supervisor.
    pub async fn ensure_covers(
        &mut self,
        start: block::Height,
        end: block::Height,
    ) -> Result<(), ConsumeError> {
        let first_index = chunk_index_for_height(start);
        let last_index = chunk_index_for_height(end);
        // `chunk_hashes.len() >= 1` for a valid spec; the count is far below
        // `u32::MAX`, so this cast never truncates.
        let max_index = self.spec.chunk_hashes.len().saturating_sub(1) as u32;

        for index in first_index..=last_index.min(max_index) {
            self.ensure_chunk(index).await?;
        }

        Ok(())
    }

    /// Ensures the chunk at `index` is resident: returns immediately if it is
    /// already cached, otherwise reuses a chunk already stored in the
    /// `known_hash_chunk` column family, or reads it from the artifact
    /// directory, verifies it against the pinned hash, persists it to the CF,
    /// and caches it.
    ///
    /// The artifact directory is read-only and local, so a failed read is not
    /// retried: a missing, oversized, or hash-mismatched chunk file fails the
    /// same way every time, and the precise error (the missing path, or the
    /// expected/actual SHA-256) propagates to the supervisor, which surfaces
    /// deterministic artifact errors to the operator instead of restarting
    /// (see [`ConsumeError::is_deterministic`]).
    ///
    /// The state CF is consulted first so a chunk persisted by a previous run (or
    /// present because this node is partly synced) is reused without any file
    /// read. A verified chunk is the exact content-addressed bytes, persisted to
    /// the CF so a restart reads it back without touching the artifact
    /// directory; a persist failure is not fatal (the chunk is already trusted
    /// in RAM).
    pub async fn ensure_chunk(&mut self, index: u32) -> Result<(), ConsumeError> {
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
        // error here is not fatal: fall through to an artifact-directory read.
        match self.read_local_chunk(index).await {
            Ok(Some(bytes)) => {
                if let Ok(verified) = verify_chunk_bytes(self.spec, index, bytes) {
                    self.chunks.insert(index, verified);
                    return Ok(());
                }
                // A locally-stored chunk that fails verification is corrupt; fall
                // through to the artifact directory rather than trusting it.
            }
            Ok(None) => {}
            Err(error) => {
                debug!(
                    %error,
                    index,
                    "reading a known-hash chunk from the local state failed; \
                     reading the artifact directory",
                );
            }
        }

        let bytes = self.read_artifact_chunk(index).await?;
        let verified = verify_chunk_bytes(self.spec, index, bytes)?;

        // Persist the verified bytes so a restart reads them back without
        // touching the artifact directory. A persist failure is not fatal — the
        // chunk is already trusted in RAM — so it is logged and the prime
        // succeeds.
        if let Err(error) = self.persist_chunk(index, &verified).await {
            warn!(
                %error,
                index,
                "failed to persist a verified known-hash chunk to the state; \
                 it is cached in RAM but a restart will re-read it",
            );
        }
        self.chunks.insert(index, verified);

        Ok(())
    }

    /// Reads the chunk at `index` from the local `known_hash_chunk` column
    /// family via the state service, or `None` if it is not stored.
    async fn read_local_chunk(&self, index: u32) -> Result<Option<Vec<u8>>, ConsumeError> {
        let response = self
            .state
            .clone()
            .oneshot(zs::Request::KnownHashChunk(index))
            .await
            .map_err(ConsumeError::StateService)?;

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
            .map_err(ConsumeError::StateService)?;

        Ok(())
    }

    /// Reads the whole chunk at `index` from the artifact directory, on a
    /// blocking thread (a chunk file is a multi-MiB read).
    ///
    /// The read is bounded by
    /// [`MAX_V2_CHUNK_BYTES`](local::MAX_V2_CHUNK_BYTES) (enforced inside
    /// [`LocalSnapshotSource::read_chunk`]) so a wrong or corrupt file cannot
    /// grow the buffer without bound; the caller's SHA-256 gate then verifies
    /// the content against the pinned `chunk_hashes[index]` constant.
    async fn read_artifact_chunk(&self, index: u32) -> Result<Vec<u8>, ConsumeError> {
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || source.read_chunk(index))
            .await
            .map_err(ConsumeError::ReadTask)?
            .map_err(ConsumeError::LocalSource)
    }
}

impl<ZS> HashSource for CfHashSource<ZS>
where
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + '_>>
    {
        Box::pin(async move {
            CfHashSource::ensure_covers(self, start, end)
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

#[cfg(test)]
mod tests;
