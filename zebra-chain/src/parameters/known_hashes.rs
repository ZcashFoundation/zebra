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
#[derive(Copy, Clone, Debug)]
pub struct KnownHashListSpec {
    /// The highest height covered by the list.
    pub max_height: block::Height,

    /// The number of block hashes in each chunk file (except the last).
    ///
    /// Pins the asset format: the windowed-residency invariant requires the
    /// engine's maximum window span to stay below this (design doc §6.4).
    pub chunk_blocks: u32,

    /// SHA-256 of each hash chunk file, as lowercase hex, in chunk order.
    pub chunk_hashes: &'static [&'static str],

    /// The chunk file name prefix, e.g. `main-known-hashes` for
    /// `main-known-hashes-00.bin`.
    pub file_prefix: &'static str,

    /// SHA-256 of the per-block size-hint array, as lowercase hex.
    ///
    /// `None` until the size-hint asset for this network has been generated
    /// and shipped (see the design doc §6.2).
    pub size_hint_hash: Option<&'static str>,
}

/// The Mainnet every-block known-hash list: 3,358,432 hashes
/// (heights 0..=3,358,431) in 23 chunks.
pub const MAINNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(3_358_431),
    chunk_blocks: 150_000,
    file_prefix: "main-known-hashes",
    // TODO(ibd-engine E2): size hints generated from a synced node.
    size_hint_hash: None,
    chunk_hashes: &[
        "3c8dd4f9cb86334aed0796fb795a3822bb89fda2a93c98b3a1f7d092a04ac4bb",
        "6840720c2e34b8cb32625ee169707d1d98981df6e14e48104b9ba6206519a8c8",
        "ed7ce0da0458c79e64384a8c3bb0ef1ca5796a2faa4dc0cdbf8a38158072cae2",
        "ae65a186dc54f6d4945b2de8fe25431b2238a3d199d1a61d6fd70e7e74441dc0",
        "2fa2815f13502c15d7c3ad4f3e2c76c0ddf96628a9a377d4613fa6fda3eb1ab0",
        "db2beab3abe5f781a7d06363e80571a89fa67add59b6a706e0d214689038f721",
        "5610c6a4ebef8c813055601314905216d7488c91bd0d0d83437242fbb3464346",
        "68b6ce9cf856d27b361ccedcafb047b55c8dbbfcaf30684953a7daf2d70df52f",
        "f9c62df1efb218f236d5195e3e5943dfbabe94b853f30b6b50a4890154ab4e6a",
        "6704e94e301bb2de6d9732c511b777ac4e083713d8e6ce1074257a12ef523e60",
        "15734d93c3e82e0fecd02833a288d32c515fa30811095e8f98f7d98c5ad08d4f",
        "36f3accf4f69f244fe59c8110688972d3f2202d185b3ac3f0c75accbfdf7475e",
        "7ba72656904c85984914d4e119db689b549ac6acfee86825bcd3d421af7e84ef",
        "e3b4001c714c52502a63893c38cc88a22d9e01c5084b6523e601b375cd85956e",
        "43ff830a861b44544e88fbce64c5a3fbbe3e67f5af4c6982afc1097c191da637",
        "79d005f596bd2c711933560d9b4a33cd904c451e4b6df23facc98596830d1e18",
        "65a554e1df7de35e9e4b2ae1a1de316afb9110afef37ba2c40565f1b77a4b06f",
        "ea870fbd205f346e9f90a575786b8fca9387c1cdebdba7f28418f66a2adc12cd",
        "538c8ed6cc2f539a7afbf9ed1832dd8c42ce5dc1c9802608c55c41b8f5c062bd",
        "5704d2c80866de97ba63856bd116c38c249466fdd7e5d0f219fb10197df57573",
        "2a1a12a53d02735f060cbf535163264afcdabb3ce080978eca8c6fb8e4dc7524",
        "0e8a98342e896a77ffb434d427e4ce2484812aa0407df4aedb274b40a0df7eba",
        "15e8ec07d2cd300d7de1c2479faf94785a150ef1cbd208f6eb5a97990990140a",
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
        format!("{}-{index:02}.bin", self.file_prefix)
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
         (the spec pins {spec_len} total hashes; assets and spec disagree)"
    )]
    ChunkLength {
        /// The chunk file path.
        path: PathBuf,
        /// The expected length in bytes.
        expected: u64,
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
        if chunk.len() as u64 != expected_len {
            return Err(KnownHashError::ChunkLength {
                path,
                expected: expected_len,
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
