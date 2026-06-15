//! Every-block known-hash lists for the known-hash initial sync engine.
//!
//! The list pins the hash of every block up to [`KnownHashListSpec::max_height`],
//! so initial sync can download blocks directly by hash instead of discovering
//! them from peers. It is far too large to compile into the binary (~103 MB on
//! Mainnet), so it ships as chunked data files which are integrity-checked
//! against hard-coded SHA-256 constants at load time. The constants are the
//! trust root, reviewed like the existing checkpoint list.
//!
//! Memory residency is windowed: [`KnownHashList::verify_assets`] reads and
//! checks every chunk once at startup and drops the bytes; during sync at most
//! two chunks (~10 MB) are resident, following the sync frontier; callers drop
//! the whole list once the chain tip passes `max_height`.
//!
//! See `docs/design/known-hash-ibd.md` §6 for the full design.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{block, parameters::Network};

#[cfg(test)]
mod tests;

/// The number of bytes used to encode a single block hash.
///
/// Each hash is stored in Zebra's internal serialized byte order (the `.0`
/// field of [`block::Hash`]), with no header, delimiter, or byte reversal.
const HASH_BYTES: usize = 32;

/// The quantum for the per-block size hints embedded in chunk files
/// (design doc §6.2): `MAX_BLOCK_BYTES.div_ceil(255)` = 7,844 bytes.
///
/// A hint byte `w` in `1..=255` means the block's serialized size is at most
/// `w × SIZE_HINT_UNIT` bytes, so hints are always upper bounds. This is the
/// single source of truth shared by the asset emitter (`zebra-utils`, which
/// quantizes sizes into hints) and the IBD engine (`zebrad`, which
/// dequantizes hints into byte-budget bounds), so the two can't disagree.
//
// 7,844 fits in a u32; the const assert below pins the value.
pub const SIZE_HINT_UNIT: u32 = crate::block::MAX_BLOCK_BYTES.div_ceil(255) as u32;

const _: () = assert!(SIZE_HINT_UNIT == 7_844);
const _: () = assert!(255 * SIZE_HINT_UNIT as u64 >= crate::block::MAX_BLOCK_BYTES);

/// The number of block hashes in each chunk file (except the last): 150,000
/// 32-byte hashes, 4.8 MB of hashes per full chunk.
///
/// Like [`SIZE_HINT_UNIT`], this is the single source of truth shared by the
/// asset emitter (`zebra-utils`) and the bundled specs the loader verifies
/// against, so the two can't disagree.
pub const HASHES_PER_CHUNK: u32 = 150_000;

/// Returns the chunk file name for `file_prefix` and chunk `index`:
/// `<file_prefix>-NN.bin` (e.g. `main-known-hashes-00.bin`).
///
/// The single source of the file-name format, shared by the asset emitter
/// (`zebra-utils`) and the loader's asset search.
pub fn chunk_file_name(file_prefix: &str, index: usize) -> String {
    format!("{file_prefix}-{index:02}.bin")
}

/// The development-tree fallback directory holding the bundled chunk files,
/// baked in at build time.
///
/// Only used as the last entry in the [`KnownHashList::resolve_dir`] search
/// order, so installed binaries resolve their assets from the config override,
/// executable-adjacent directories, or the platform data directory instead.
const DEV_KNOWN_HASHES_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/src/parameters/known_hashes");

/// A compile-time description of a network's known-hash list assets.
///
/// `max_height` is a reviewed constant — never derived from file sizes — so
/// consumers that only need the list's coverage (the commit gate floor, state
/// initialization, progress reporting) require no chunk I/O at all, and a
/// truncated or extended asset set is detected as a hard mismatch at load.
///
/// # Chunk file format
///
/// A chunk holding `n` blocks is either `n × 32` bytes (one internal-order
/// block hash per block), or `n × 33` bytes: the `n` hashes followed by one
/// size-hint byte per block, in the same height order (design doc §6.2). The
/// pinned SHA-256 constants distinguish the formats, so hints can ship
/// per-chunk as size data becomes available.
#[derive(Copy, Clone, Debug)]
pub struct KnownHashListSpec {
    /// The highest height covered by the list.
    pub max_height: block::Height,

    /// The number of block hashes in each chunk file (except the last).
    ///
    /// Pins the asset format: the windowed-residency invariant requires the
    /// engine's maximum window span to stay below this (design doc §6.4).
    pub chunk_blocks: u32,

    /// SHA-256 of each chunk file, as lowercase hex, in chunk order.
    pub chunk_hashes: &'static [&'static str],

    /// The chunk file name prefix, e.g. `main-known-hashes` for
    /// `main-known-hashes-00.bin`.
    pub file_prefix: &'static str,
}

/// The Mainnet every-block known-hash list: 3,373,207 hashes
/// (heights 0..=3,373,206) in 23 chunks, each embedding per-block size hints.
pub const MAINNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(3_373_206),
    chunk_blocks: HASHES_PER_CHUNK,
    file_prefix: "main-known-hashes",
    chunk_hashes: &[
        "5c9719ada92cd27e622be82b58f6d8ead1270f0af1b5a8644021b80512db3e90",
        "d4bab1830873891534e9e96855ea39aeb8b6d78b7658c6a688381f17a492837b",
        "ebb26bbb3b97329525661ebe5426ab34ffc9c298cf7385deed99b1a73b2ae843",
        "6031454d8e7f35e421382646bbd5786e30cc8f56b72580f8a2c6cbbe43638880",
        "e3b2fc594b317337110003846fa4af3f343e54cb4d3c701165feb504365a685a",
        "220d9361c3c301cc5cd6fa1199c0e906002b1d5081cce73aa95b1675ce0ffcf6",
        "3f5d1bf1a2a63b9feb4186c3e30726048d933b00069aee07e7f832f57a03c165",
        "c62cf58ee51946e8d9c56925eb5bb12ac726db11bf95512e7fbf21a8a1e50d02",
        "6ae103db21d767b84f5434b931c0bd15bbe42f2c07763768066efe6043b45ef7",
        "ea1234be1244f3d92b78e00feb7da4a2453e2830de53d9758a38d5c8b591ffee",
        "3c8fdab2136ddba3c44a5328c0bb80e89c96f8cb2e8d03f4d3697e563079a443",
        "03446d4bababe433b48b4f2f40add345e8c7e3130d947cabd8d3d77fd7c41d38",
        "58cf63ddb267f6ce7cba94f96afe64a24f6cceb318060afd29d4e9dbbf6574b5",
        "300a8ba12a7d0fd10fbddaafa643decac7fabe76849a38e55663fdd8bdd8ca5d",
        "ba134982f8c8f339ee1f408d59254c76329fb07a158697f725b7502e84fd4926",
        "9ab753b04ee8bd18c795c0bb94c4523fb80a077d0787b62462cf8536f10eeb73",
        "7d53879f0d911c4dd2d0dfb95269ccbdc55a28599a9a63002a3012333c454e50",
        "ed87b7012bace58ea1145abb50633a43cebcc2d2cbb552cb5be3fca9fcf4a3e4",
        "fc6a82d97e3c6caf3d220d0eaa5022b711cd26e848d6544cc2371d883d9519a1",
        "f5cbb51028b8acd3d618e7588e8920a51d5af28b23f0e9d27378dc10624c0b9f",
        "c6df924a3c21ee4eac81ef73a1b2b229d7c75a3b549b49dc6c054c76e9439b03",
        "d444c2e1afdcd6635c8707e0d302a3064fc5dacb6a96e09c18a56969429b11e3",
        "651f6924710b43b9e161186ba81bdfddc12f2217d18f4c53eae9cdeb05c1a07a",
    ],
};

impl KnownHashListSpec {
    /// Returns the known-hash list spec for `network`, if one is bundled.
    ///
    /// Currently Mainnet only; the Testnet list is assembled in a later phase
    /// (design doc §7.4).
    pub fn for_network(network: &Network) -> Option<&'static KnownHashListSpec> {
        match network {
            Network::Mainnet => Some(&MAINNET_KNOWN_HASHES),
            _ => None,
        }
    }

    /// The number of block hashes in the list.
    pub fn len(&self) -> u64 {
        u64::from(self.max_height.0) + 1
    }

    /// Whether the list is empty (never true for a valid spec).
    pub fn is_empty(&self) -> bool {
        self.chunk_hashes.is_empty()
    }

    /// The number of hashes expected in chunk `index`.
    fn chunk_len(&self, index: usize) -> u64 {
        let full_chunks = (self.chunk_hashes.len() - 1) as u64;
        if (index as u64) < full_chunks {
            u64::from(self.chunk_blocks)
        } else {
            self.len() - full_chunks * u64::from(self.chunk_blocks)
        }
    }

    /// The chunk file name for chunk `index`.
    fn chunk_file_name(&self, index: usize) -> String {
        chunk_file_name(self.file_prefix, index)
    }
}

/// Errors loading or verifying known-hash list assets.
#[derive(Error, Debug)]
pub enum KnownHashError {
    /// No asset directory contained the chunk files.
    #[error(
        "known-hash chunk files not found; searched: {searched:?}. \
         Set the [sync] known_hash_list_dir config, or place the files next to \
         the zebrad binary or in the platform data directory"
    )]
    AssetsNotFound {
        /// The directories that were searched.
        searched: Vec<PathBuf>,
    },

    /// A chunk file could not be read.
    #[error("could not read known-hash chunk {path}: {error}")]
    ChunkRead {
        /// The chunk file path.
        path: PathBuf,
        /// The underlying I/O error.
        error: std::io::Error,
    },

    /// A chunk file failed its SHA-256 integrity check.
    #[error(
        "known-hash chunk {path} failed its SHA-256 integrity check: \
         expected {expected}, got {actual} — the file is corrupt or tampered"
    )]
    ChunkHashMismatch {
        /// The chunk file path.
        path: PathBuf,
        /// The pinned hash from the spec constant.
        expected: String,
        /// The hash of the bytes on disk.
        actual: String,
    },

    /// A chunk file's length does not match the spec.
    #[error(
        "known-hash chunk {path} is {actual} bytes, expected {expected} \
         (hash-only) or {expected_with_hints} (with embedded size hints; \
         the spec pins {spec_len} total hashes; assets and spec disagree)"
    )]
    ChunkLength {
        /// The chunk file path.
        path: PathBuf,
        /// The expected hash-only length in bytes.
        expected: u64,
        /// The expected length in bytes with embedded size hints.
        expected_with_hints: u64,
        /// The actual length in bytes.
        actual: u64,
        /// The total number of hashes pinned by the spec.
        spec_len: u64,
    },

    /// The first hash in the list does not match the network genesis hash.
    #[error(
        "known-hash list genesis mismatch: list starts with {list_genesis}, \
         network genesis is {network_genesis}"
    )]
    GenesisMismatch {
        /// The hash at height 0 in the list.
        list_genesis: block::Hash,
        /// The network's genesis hash.
        network_genesis: block::Hash,
    },
}

/// A windowed view over a network's every-block known-hash list.
///
/// Holds at most two resident chunks (the one containing the most recent
/// lookup, and one neighbor), each re-verified against its pinned SHA-256 on
/// load. Drop the whole struct once the chain tip passes
/// [`KnownHashListSpec::max_height`].
pub struct KnownHashList {
    spec: &'static KnownHashListSpec,
    dir: PathBuf,
    /// Resident chunks: `(chunk index, raw hash bytes)`, most recently used
    /// last. Never more than [`Self::MAX_RESIDENT_CHUNKS`] entries; the raw
    /// `Vec<u8>` read from disk is kept as-is and indexed by offset, avoiding
    /// a second multi-megabyte copy per chunk load.
    resident: Vec<(usize, Vec<u8>)>,
}

impl KnownHashList {
    /// The maximum number of chunks held in memory at once.
    const MAX_RESIDENT_CHUNKS: usize = 2;

    /// The asset search order, highest priority first:
    /// the config override, executable-adjacent directories, the platform
    /// data directory, then the development tree.
    fn search_dirs(config_dir: Option<&Path>) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        if let Some(dir) = config_dir {
            dirs.push(dir.to_owned());
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                dirs.push(exe_dir.join("known-hashes"));
                dirs.push(exe_dir.join("../share/zebrad/known-hashes"));
            }
        }

        if let Some(data_dir) = dirs::data_dir() {
            dirs.push(data_dir.join("zebrad/known-hashes"));
        }

        dirs.push(PathBuf::from(DEV_KNOWN_HASHES_DIR));

        dirs
    }

    /// Finds the asset directory for `spec`, searching [`Self::search_dirs`]
    /// for the first directory containing chunk `00`.
    fn resolve_dir(
        spec: &KnownHashListSpec,
        config_dir: Option<&Path>,
    ) -> Result<PathBuf, KnownHashError> {
        let searched = Self::search_dirs(config_dir);

        searched
            .iter()
            .find(|dir| dir.join(spec.chunk_file_name(0)).is_file())
            .cloned()
            .ok_or(KnownHashError::AssetsNotFound { searched })
    }

    /// Opens the known-hash list for `network`, verifying **every** chunk's
    /// SHA-256, length, and the genesis hash, then dropping the bytes.
    ///
    /// Returns `Ok(None)` if no list is bundled for `network`.
    ///
    /// This reads the full asset set once (~103 MB on Mainnet) — cheap disk
    /// I/O that fails fast on tampered or mismatched assets — but retains no
    /// chunk in memory; lookups load chunks on demand.
    pub fn open(
        network: &Network,
        config_dir: Option<&Path>,
    ) -> Result<Option<Self>, KnownHashError> {
        let Some(spec) = KnownHashListSpec::for_network(network) else {
            return Ok(None);
        };

        Self::open_spec(spec, network, config_dir).map(Some)
    }

    /// [`Self::open`] for a specific spec; see there.
    fn open_spec(
        spec: &'static KnownHashListSpec,
        network: &Network,
        config_dir: Option<&Path>,
    ) -> Result<Self, KnownHashError> {
        let dir = Self::resolve_dir(spec, config_dir)?;

        Self::verify_assets(spec, network, &dir)?;

        Ok(Self {
            spec,
            dir,
            resident: Vec::new(),
        })
    }

    /// Reads and verifies every chunk against the spec, then drops the bytes.
    #[allow(clippy::unwrap_in_result)]
    fn verify_assets(
        spec: &KnownHashListSpec,
        network: &Network,
        dir: &Path,
    ) -> Result<(), KnownHashError> {
        for index in 0..spec.chunk_hashes.len() {
            let chunk = Self::read_chunk_raw(spec, dir, index)?;

            if index == 0 {
                let list_genesis = block::Hash(
                    chunk[..HASH_BYTES]
                        .try_into()
                        .expect("chunk length is a verified non-zero multiple of HASH_BYTES"),
                );
                let network_genesis = network.genesis_hash();
                if list_genesis != network_genesis {
                    return Err(KnownHashError::GenesisMismatch {
                        list_genesis,
                        network_genesis,
                    });
                }
            }
        }

        Ok(())
    }

    /// Reads chunk `index`, verifying its length and SHA-256 against the spec.
    ///
    /// Accepts both chunk formats: hash-only (32 bytes per block) and with
    /// embedded size hints (33 bytes per block).
    fn read_chunk_raw(
        spec: &KnownHashListSpec,
        dir: &Path,
        index: usize,
    ) -> Result<Vec<u8>, KnownHashError> {
        let path = dir.join(spec.chunk_file_name(index));

        let chunk = fs::read(&path).map_err(|error| KnownHashError::ChunkRead {
            path: path.clone(),
            error,
        })?;

        let expected_len = spec.chunk_len(index) * HASH_BYTES as u64;
        let expected_len_with_hints = spec.chunk_len(index) * (HASH_BYTES + 1) as u64;
        if chunk.len() as u64 != expected_len && chunk.len() as u64 != expected_len_with_hints {
            return Err(KnownHashError::ChunkLength {
                path,
                expected: expected_len,
                expected_with_hints: expected_len_with_hints,
                actual: chunk.len() as u64,
                spec_len: spec.len(),
            });
        }

        let actual = hex::encode(Sha256::digest(&chunk));
        let expected = spec.chunk_hashes[index];
        if actual != expected {
            return Err(KnownHashError::ChunkHashMismatch {
                path,
                expected: expected.to_owned(),
                actual,
            });
        }

        Ok(chunk)
    }

    /// The highest height covered by the list.
    pub fn max_height(&self) -> block::Height {
        self.spec.max_height
    }

    /// Returns the pinned block hash for `height`, or `None` past the end of
    /// the list.
    ///
    /// Loads (and re-verifies) the chunk containing `height` if it is not
    /// resident, evicting the least recently used chunk beyond the two-chunk
    /// residency limit. Sync-only sequential access keeps this to one chunk
    /// load per 150,000 heights.
    #[allow(clippy::unwrap_in_result)]
    pub fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, KnownHashError> {
        if height > self.spec.max_height {
            return Ok(None);
        }

        let chunk_index = (height.0 / self.spec.chunk_blocks) as usize;
        let offset = (height.0 % self.spec.chunk_blocks) as usize * HASH_BYTES;

        let chunk = self.resident_chunk(chunk_index)?;

        let hash = chunk[offset..offset + HASH_BYTES]
            .try_into()
            .expect("chunk length is a verified multiple of HASH_BYTES");

        Ok(Some(block::Hash(hash)))
    }

    /// Returns the resident hashes for `chunk_index`, loading it if needed.
    #[allow(clippy::unwrap_in_result)]
    fn resident_chunk(&mut self, chunk_index: usize) -> Result<&[u8], KnownHashError> {
        // Move an already-resident chunk to the most-recently-used position.
        if let Some(pos) = self.resident.iter().position(|(i, _)| *i == chunk_index) {
            let entry = self.resident.remove(pos);
            self.resident.push(entry);
        } else {
            let raw = Self::read_chunk_raw(self.spec, &self.dir, chunk_index)?;

            self.resident.push((chunk_index, raw));

            if self.resident.len() > Self::MAX_RESIDENT_CHUNKS {
                self.resident.remove(0);
            }
        }

        Ok(self
            .resident
            .last()
            .map(|(_, raw)| raw.as_slice())
            .expect("just inserted or moved the chunk to the back"))
    }

    /// Returns the embedded size hint for `height`, or `None` past the end
    /// of the list or when `height`'s chunk is hash-only (design doc §6.2).
    ///
    /// A hint byte `w` in `1..=255` means the block's serialized size is at
    /// most `w` size-hint units. Loads the chunk containing `height` like
    /// [`hash`](Self::hash).
    pub fn size_hint(&mut self, height: block::Height) -> Result<Option<u8>, KnownHashError> {
        if height > self.spec.max_height {
            return Ok(None);
        }

        let chunk_index = (height.0 / self.spec.chunk_blocks) as usize;
        let chunk_blocks = self.spec.chunk_len(chunk_index) as usize;
        let offset = (height.0 % self.spec.chunk_blocks) as usize;

        let chunk = self.resident_chunk(chunk_index)?;

        // Hash-only chunks are `32 × n` bytes; hinted chunks append one hint
        // byte per block after the `32 × n` hash section.
        if chunk.len() == chunk_blocks * HASH_BYTES {
            return Ok(None);
        }

        Ok(Some(chunk[chunk_blocks * HASH_BYTES + offset]))
    }

    /// Drops resident chunks that only cover heights below `height`.
    ///
    /// Call as the sync frontier advances so memory follows the frontier.
    pub fn release_below(&mut self, height: block::Height) {
        let keep_from = (height.0 / self.spec.chunk_blocks) as usize;
        self.resident.retain(|(index, _)| *index >= keep_from);
    }

    /// The number of currently resident chunks (for tests and metrics).
    pub fn resident_chunks(&self) -> usize {
        self.resident.len()
    }
}

impl std::fmt::Debug for KnownHashList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownHashList")
            .field("max_height", &self.spec.max_height)
            .field("dir", &self.dir)
            .field(
                "resident_chunks",
                &self.resident.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            )
            .finish()
    }
}
