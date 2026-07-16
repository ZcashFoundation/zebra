//! A local-file source for the snapshot-consume artifacts.
//!
//! In snapshot-consume mode the engine reads each artifact (known-hash chunks,
//! note commitment trees, the unspent-output set, the address-balance set, and
//! the chain value pools) from a local directory named by
//! [`sync.known_hash_local_source_dir`](crate::components::sync::Config::known_hash_local_source_dir):
//! the directory the installer downloaded from the release assets, or one laid
//! out locally by `emit-snapshot --emit-files`. Every artifact is verified
//! against the pinned SHA-256 constants regardless of where the directory came
//! from (`docs/design/snapshot-distribution.md`). Blocks themselves still come
//! over normal P2P / known-hash.
//!
//! # File layout
//!
//! The directory (written by `emit-snapshot --emit-files --out-dir <dir>`) is:
//!
//! ```text
//! <dir>/
//! ├── MANIFEST.txt              human-readable layout + provenance (network, H_max)
//! ├── chunks/
//! │   ├── chunk-00000.bin       the exact v2 chunk bytes for index 0
//! │   ├── chunk-00001.bin       … one file per chunk index, zero-padded to 5 digits
//! │   └── …
//! ├── sapling-trees.bin         (height u32 LE, len u32 LE, frontier-bytes)* sorted by height
//! ├── orchard-trees.bin         same record layout for the orchard tree
//! ├── unspent-output-locations.bin   the sorted unspent-output-location set (raw bytes)
//! ├── address-balances.bin           the sorted address-balance set (raw bytes)
//! └── chain-value-pools.bin          the 40-byte H_max ValueBalance
//! ```
//!
//! The chunk files hold the **exact** `chunk_v2` bytes whose SHA-256 is the pinned
//! `chunk_hashes[index]` constant, so a chunk read from a file verifies through
//! the [`verify_chunk_bytes`](super::verify_chunk_bytes) gate. The tree records
//! hold the canonical `tree.as_bytes()` serialization, so the consumer's
//! recomputed `.root()` matches the chunk's recorded root. The two set files
//! hold the sorted set bytes whose SHA-256 matches the pinned set hash. The
//! value-pool file holds the 40-byte `ValueBalance::to_bytes` encoding.
//!
//! See [`SnapshotSource`](super::SnapshotSource) for the single dispatch point
//! the chunk reads go through.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use thiserror::Error;

use zebra_chain::parameters::known_hashes::HASHES_PER_CHUNK;
use zebra_state::ShieldedPool;

/// The sub-directory under the local source dir holding the per-index chunk
/// files.
pub const CHUNKS_SUBDIR: &str = "chunks";

/// The manifest file documenting the layout and provenance.
pub const MANIFEST_FILE: &str = "MANIFEST.txt";

/// The sapling note-commitment-tree records file name.
pub const SAPLING_TREES_FILE: &str = "sapling-trees.bin";

/// The orchard note-commitment-tree records file name.
pub const ORCHARD_TREES_FILE: &str = "orchard-trees.bin";

/// The sorted unspent-output-location set file name.
pub const UNSPENT_OUTPUTS_FILE: &str = "unspent-output-locations.bin";

/// The sorted address-balance set file name.
pub const ADDRESS_BALANCES_FILE: &str = "address-balances.bin";

/// The H_max chain value pools file name (a 40-byte `ValueBalance::to_bytes`).
pub const CHAIN_VALUE_POOLS_FILE: &str = "chain-value-pools.bin";

/// The file name for the chunk at `index`, e.g. `chunk-00003.bin`.
///
/// The index is zero-padded to five digits so the files sort lexicographically in
/// index order; five digits cover every plausible chunk count (a 150,000-block
/// chunk span means index 99,999 covers height ~15 billion).
pub fn chunk_file_name(index: u32) -> String {
    format!("chunk-{index:05}.bin")
}

/// The maximum serialized size of a v2 known-hash chunk, used to refuse an
/// oversized chunk artifact file before reading it into memory.
///
/// A full chunk holds `HASHES_PER_CHUNK` blocks. The v2 layout is the 16-byte
/// header, `n × 32` hashes, `n × 1` size hints, then two tree-root sections.
/// Each tree section has at most one record per height (`4 + 32` bytes), giving a
/// generous upper bound of `16 + n × (32 + 1) + 2 × (4 + n × 36)`. An honest
/// chunk always fits; a corrupt or wrong file cannot grow the read past it.
pub const MAX_V2_CHUNK_BYTES: u64 = 16
    + HASHES_PER_CHUNK as u64 * (HASH_BYTES_U64 + 1)
    + 2 * (4 + HASHES_PER_CHUNK as u64 * (4 + HASH_BYTES_U64));

/// The byte length of a single 32-byte hash, as a `u64`, for the
/// [`MAX_V2_CHUNK_BYTES`] bound.
const HASH_BYTES_U64: u64 = 32;

/// Errors reading a snapshot artifact from the local-file source.
#[derive(Debug, Error)]
pub enum LocalSourceError {
    /// A required artifact file is missing from the local source directory.
    #[error("the local snapshot source has no {artifact} file at {}", path.display())]
    Missing {
        /// A short description of the artifact (`chunk 3`, `sapling-trees.bin`).
        artifact: String,
        /// The path that was expected to exist.
        path: PathBuf,
    },

    /// An artifact file could not be read.
    #[error("could not read the local snapshot {artifact} file at {}: {source}", path.display())]
    Io {
        /// A short description of the artifact.
        artifact: String,
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A tree records file is structurally malformed (a truncated record header
    /// or a length running past the end of the file).
    #[error("the local snapshot {pool:?}-trees file at {} is malformed: {reason}", path.display())]
    MalformedTrees {
        /// The shielded pool.
        pool: ShieldedPool,
        /// The path of the malformed file.
        path: PathBuf,
        /// Why the file is malformed.
        reason: String,
    },

    /// An artifact file is larger than any valid instance of its artifact could
    /// be, so it is refused before it is read into memory.
    #[error(
        "the local snapshot {artifact} file at {} is {len} bytes, over the {cap}-byte \
         maximum for a valid artifact; refusing to read it",
        path.display()
    )]
    Oversized {
        /// A short description of the artifact (`chunk 3`).
        artifact: String,
        /// The path of the oversized file.
        path: PathBuf,
        /// The file's reported length.
        len: u64,
        /// The maximum valid length.
        cap: u64,
    },
}

/// A read-only view over a directory of snapshot artifacts laid out by
/// `emit-snapshot --emit-files` (and downloaded by the installer).
///
/// Cloning is cheap (the directory path plus shared tree caches), and clones
/// share the parsed tree caches. All reads are synchronous file I/O; the
/// consumers wrap each read in `spawn_blocking` (the engine's tree-fetch stage
/// and the chunk source both do). Verification against the pinned hashes is the
/// caller's job — this type only reads the raw bytes.
#[derive(Clone, Debug)]
pub struct LocalSnapshotSource {
    /// The root directory of the local artifact set.
    dir: PathBuf,

    /// The parsed sapling tree records, shared across clones and parsed at most
    /// once: the engine reads one tree per updating height, so re-parsing the
    /// whole records file per read would be quadratic in the record count.
    sapling_trees: Arc<OnceLock<Arc<BTreeMap<u32, Vec<u8>>>>>,

    /// The parsed orchard tree records, like
    /// [`sapling_trees`](Self::sapling_trees).
    orchard_trees: Arc<OnceLock<Arc<BTreeMap<u32, Vec<u8>>>>>,
}

impl LocalSnapshotSource {
    /// Returns a local source rooted at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            sapling_trees: Arc::new(OnceLock::new()),
            orchard_trees: Arc::new(OnceLock::new()),
        }
    }

    /// The root directory of this source.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the raw v2 chunk bytes for `index` from `chunks/chunk-<index>.bin`.
    ///
    /// Returns the exact bytes written by the emitter, suitable for the
    /// [`verify_chunk_bytes`](super::verify_chunk_bytes) gate (SHA-256 vs the
    /// pinned `chunk_hashes[index]`). A missing file is an error, not a silent
    /// miss: a configured local source that lacks a needed chunk is a
    /// misconfiguration the operator must fix. A file over
    /// [`MAX_V2_CHUNK_BYTES`] is refused before it is read into memory: no
    /// valid chunk is that large, so the bound only rejects wrong or corrupt
    /// files.
    pub fn read_chunk(&self, index: u32) -> Result<Vec<u8>, LocalSourceError> {
        let path = self.dir.join(CHUNKS_SUBDIR).join(chunk_file_name(index));

        if let Ok(metadata) = fs::metadata(&path) {
            if metadata.len() > MAX_V2_CHUNK_BYTES {
                return Err(LocalSourceError::Oversized {
                    artifact: format!("chunk {index}"),
                    path,
                    len: metadata.len(),
                    cap: MAX_V2_CHUNK_BYTES,
                });
            }
        }

        read_file(&path, || format!("chunk {index}"))
    }

    /// Returns the canonical note-commitment-tree frontier bytes for `pool` as
    /// of `height`, from the relevant `*-trees.bin` records file.
    ///
    /// The records file is read and parsed at most once per pool (via
    /// [`read_all_trees`](Self::read_all_trees)); the parsed map is cached and
    /// shared across clones, so per-height reads are map lookups. Returns
    /// `None` if no record for `height` exists (a height the artifact ships no
    /// frontier for, so the engine folds instead).
    pub fn read_tree(
        &self,
        pool: ShieldedPool,
        height: u32,
    ) -> Result<Option<Vec<u8>>, LocalSourceError> {
        let trees = self.all_trees_cached(pool)?;
        Ok(trees.get(&height).cloned())
    }

    /// Returns the shared parsed tree map for `pool`, parsing the records file
    /// on the first call.
    ///
    /// A parse or read failure is returned each call rather than cached: the
    /// error path stays slow, but a fixed file works on the next attempt. If
    /// two callers race the first parse, the first `set` wins and the loser's
    /// identical map is dropped (the file is read-only, so both parses agree).
    fn all_trees_cached(
        &self,
        pool: ShieldedPool,
    ) -> Result<Arc<BTreeMap<u32, Vec<u8>>>, LocalSourceError> {
        let cell = match pool {
            ShieldedPool::Sapling => &self.sapling_trees,
            ShieldedPool::Orchard => &self.orchard_trees,
        };

        if let Some(trees) = cell.get() {
            return Ok(trees.clone());
        }

        let trees = Arc::new(self.read_all_trees(pool)?);
        let _ = cell.set(trees.clone());
        Ok(cell.get().cloned().unwrap_or(trees))
    }

    /// Parses the whole `*-trees.bin` records file for `pool` into a map from
    /// height to the canonical frontier bytes at that height.
    ///
    /// The on-disk format is a flat sequence of
    /// `(height u32 LE, len u32 LE, len-byte frontier)` records sorted ascending
    /// by height. A truncated header or an over-long length is a hard
    /// [`LocalSourceError::MalformedTrees`] — the file was produced by the
    /// emitter, so any malformation is a local problem, never an untrusted-peer
    /// case.
    pub fn read_all_trees(
        &self,
        pool: ShieldedPool,
    ) -> Result<BTreeMap<u32, Vec<u8>>, LocalSourceError> {
        let file = match pool {
            ShieldedPool::Sapling => SAPLING_TREES_FILE,
            ShieldedPool::Orchard => ORCHARD_TREES_FILE,
        };
        let path = self.dir.join(file);
        let bytes = read_file(&path, || format!("{pool:?}-trees"))?;

        let mut trees = BTreeMap::new();
        let mut cursor = 0usize;
        while cursor < bytes.len() {
            // Each record begins with an 8-byte header: height u32 LE, len u32 LE.
            if cursor + 8 > bytes.len() {
                return Err(LocalSourceError::MalformedTrees {
                    pool,
                    path: path.clone(),
                    reason: format!("truncated record header at byte {cursor}"),
                });
            }
            // `cursor + 8 <= bytes.len()` was just checked, so both 4-byte
            // windows are in bounds and `le_u32_at` always returns `Some`.
            let height = le_u32_at(&bytes, cursor).unwrap_or(0);
            let len = le_u32_at(&bytes, cursor + 4).unwrap_or(0) as usize;
            cursor += 8;

            // The frontier bytes follow the header; an over-long length running
            // past the file end is a malformed file.
            let end = cursor.checked_add(len).filter(|&end| end <= bytes.len());
            let Some(end) = end else {
                return Err(LocalSourceError::MalformedTrees {
                    pool,
                    path: path.clone(),
                    reason: format!(
                        "record at height {height} claims {len} bytes but only \
                         {} remain",
                        bytes.len() - cursor,
                    ),
                });
            };

            trees.insert(height, bytes[cursor..end].to_vec());
            cursor = end;
        }

        Ok(trees)
    }

    /// Reads a whole snapshot set file (the unspent-output or address-balance
    /// set).
    ///
    /// `file` is one of [`UNSPENT_OUTPUTS_FILE`] / [`ADDRESS_BALANCES_FILE`].
    /// The returned bytes are the exact sorted set bytes whose SHA-256 matches
    /// the pinned set hash.
    pub fn read_set(&self, file: &str) -> Result<Vec<u8>, LocalSourceError> {
        let path = self.dir.join(file);
        read_file(&path, || file.to_string())
    }

    /// Reads the whole `chain-value-pools.bin` file (the 40-byte H_max
    /// `ValueBalance::to_bytes` encoding).
    pub fn read_chain_value_pools(&self) -> Result<Vec<u8>, LocalSourceError> {
        let path = self.dir.join(CHAIN_VALUE_POOLS_FILE);
        read_file(&path, || CHAIN_VALUE_POOLS_FILE.to_string())
    }
}

/// Reads a little-endian `u32` from the 4-byte window at `offset`, or `None` if
/// the window runs past the end of `bytes`.
///
/// Avoids a `try_into().expect()` inside a `Result`-returning parser: the caller
/// has already bounds-checked the window, so this never returns `None` in
/// practice, but failing safe keeps the parser panic-free.
fn le_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let window: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(window))
}

/// Reads `path` in full, mapping a not-found error to
/// [`LocalSourceError::Missing`] and any other I/O error to
/// [`LocalSourceError::Io`], labelling both with `artifact()`.
fn read_file(path: &Path, artifact: impl Fn() -> String) -> Result<Vec<u8>, LocalSourceError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(LocalSourceError::Missing {
                artifact: artifact(),
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(LocalSourceError::Io {
            artifact: artifact(),
            path: path.to_path_buf(),
            source,
        }),
    }
}
