//! Every-block known-hash lists for the known-hash sync engine.
//!
//! The list pins the hash of every block up to [`KnownHashListSpec::max_height`],
//! so sync can download blocks directly by hash instead of discovering them
//! from peers. The chunk data itself is far too large to compile into the
//! binary (~103 MB on Mainnet) and is never shipped: nodes download chunks
//! from peers over the version 2 protocol — `get-object` addressed by the
//! pinned chunk hash, or reassembled from `get-hashes` responses — verify
//! them with [`KnownHashListSpec::verify_chunk_bytes`], and store them in the
//! state's known-hash chunk column family. Serving nodes regenerate chunk
//! bytes deterministically from their state, so the same pinned hash
//! addresses every copy.
//!
//! The pinned per-chunk SHA-256 constants are the trust root, reviewed like
//! the existing checkpoint list.

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{block, parameters::Network};

pub mod chunk_v2;

#[cfg(test)]
mod tests;

/// The number of bytes used to encode a single block hash.
///
/// Each hash is stored in Zebra's internal serialized byte order (the `.0`
/// field of [`block::Hash`]), with no header, delimiter, or byte reversal.
const HASH_BYTES: usize = 32;

/// The quantum for the per-block size hints embedded in chunks:
/// `MAX_BLOCK_BYTES.div_ceil(255)` = 7,844 bytes.
///
/// A hint byte `w` in `1..=255` means the block's serialized size is at most
/// `w × SIZE_HINT_UNIT` bytes, so hints are always upper bounds. This is the
/// single source of truth shared by the chunk generators (the state's
/// deterministic chunk emission and the `known-hashes` pinning tool in
/// `zebra-utils`) and the sync engine (`zebrad`, which dequantizes hints into
/// byte-budget bounds), so the two can't disagree.
//
// 7,844 fits in a u32; the const assert below pins the value.
pub const SIZE_HINT_UNIT: u32 = crate::block::MAX_BLOCK_BYTES.div_ceil(255) as u32;

const _: () = assert!(SIZE_HINT_UNIT == 7_844);
const _: () = assert!(255 * SIZE_HINT_UNIT as u64 >= crate::block::MAX_BLOCK_BYTES);

/// The number of block hashes in each chunk (except the last): 150,000
/// 32-byte hashes, 4.8 MB of hashes per full chunk.
///
/// Like [`SIZE_HINT_UNIT`], this is the single source of truth shared by the
/// chunk generators and the specs the downloaders verify against, so the two
/// can't disagree.
pub const HASHES_PER_CHUNK: u32 = 150_000;

/// The minimum number of notes a block must append to a shielded pool's note
/// commitment tree for that height's frontier to be recorded in the v2
/// chunks.
///
/// Below the threshold the consuming engine folds the block's notes into the
/// frontier itself, which is cheap for a handful of notes; at or above it,
/// loading the ~1.2 KiB downloaded frontier is cheaper than folding (for
/// example, Orchard blocks during the 2022–2023 transaction spam append
/// hundreds to thousands of notes each). The value trades chunk size against
/// fold CPU during the initial sync.
///
/// This threshold is part of the deterministic chunk generation, so changing
/// it changes the pinned `chunk_hashes` and requires re-pinning.
pub const TREE_FRONTIER_MIN_NOTES: u64 = 64;

/// A compile-time description of a network's known-hash list.
///
/// `max_height` is a reviewed constant — never derived from downloaded data —
/// so consumers that only need the list's coverage (the commit gate floor,
/// state initialization, progress reporting) require no chunk access at all,
/// and a truncated or extended chunk set is detected as a hard mismatch at
/// verification.
///
/// # Chunk format
///
/// [`chunk_hashes`](Self::chunk_hashes) is the SHA-256 of each chunk's
/// canonical [v2 (`ZKH2`)](chunk_v2) bytes: the framing carrying the block
/// hashes, optional per-block size hints, and the sparse sapling/orchard
/// tree-root updates. Chunk bytes are regenerated deterministically from a
/// synced node's state, so the pinned hash is the chunk's `get-object`
/// content address on every node.
///
/// The current constants predate the deterministic v2 serving path and are
/// re-pinned by the release flow (the `known-hashes` tool in `zebra-utils`)
/// before the sync engine ships; local multi-node tests pin from a trusted
/// local node instead.
#[derive(Copy, Clone, Debug)]
pub struct KnownHashListSpec {
    /// The highest height covered by the list.
    pub max_height: block::Height,

    /// The number of block hashes in each chunk (except the last).
    ///
    /// Pins the chunk format: the windowed-residency invariant requires the
    /// engine's maximum window span to stay below this.
    pub chunk_blocks: u32,

    /// SHA-256 of each chunk's bytes, as lowercase hex, in chunk order.
    ///
    /// The content-addressing trust root: downloaders hash the received (or
    /// reassembled) chunk bytes and serving nodes hash the regenerated chunk
    /// bytes, both comparing against this constant.
    pub chunk_hashes: &'static [&'static str],
}

/// The Mainnet every-block known-hash list: 3,373,207 hashes
/// (heights 0..=3,373,206) in 23 chunks.
pub const MAINNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(3_373_206),
    chunk_blocks: HASHES_PER_CHUNK,
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

/// The Testnet every-block known-hash list: 4,057,201 hashes
/// (heights 0..=4,057,200) in 28 chunks.
pub const TESTNET_KNOWN_HASHES: KnownHashListSpec = KnownHashListSpec {
    max_height: block::Height(4_057_200),
    chunk_blocks: HASHES_PER_CHUNK,
    chunk_hashes: &[
        "db46db6f5badf86de84e0fca397aa56cec951d064e5cc2774767fe5945932fec",
        "b5107840198f5cdf459f07aab9abd935233ef97a9f3e25483134f270e7b095a0",
        "345a929865ab41e51d780ef04ec63252531d5ae23737edcb34055b5b5d804095",
        "32d9a4a3da44e66cae83ff6bfb9d8076d07205f55b5dfd72de7739771f13d7d2",
        "4eccc9d2565a1a890d9e9b03dba9f38d18be8b6632dadaacf245f68ad10d409f",
        "ed862cf5062736745e693f431abf365af94b070da376e35ade88115a403091ed",
        "6d44807f17d82750a0449875ad6d9094447537d99a1e15cb8adfebbc0090c98f",
        "bfe991320ef5f84569753cd890766d3513c2c1dded004a51e20c210a1f561c89",
        "a564afe668a4c7d7c17a1e2cc2b17613fe1a909c33563bf2316a17cea42f3935",
        "140817694ab7b575c1dedc1d7cd1ffab4c5fc1eea83a995e62c0369c104bd14e",
        "ef1279b6b6d376f69f40f8bfa1b5b1adf7e5625f7b45e9bbfde22c73e8cba45b",
        "2bbf2ff53cd3903b70c64c3034c1fd6bf60101db7bc2bc471324f10990bed79a",
        "7b2ef839922b3087ea6f2983ccc169f8e1cb27c9f14188c35db595c7173932ab",
        "3de6366a13f4bcb9d371fc74f756b6eb2edfb48f7aa0d297ce1107a2d9a1708e",
        "17c25a41b6352b0ae4437252a5b6a1cb1e77f32cbbe88a0dc2fa288dc356dc7b",
        "e7fd222f427c3872ee45ca4608d04b416d458c5617af04a889537e9bb9363f80",
        "4b9da715b418dd1676fb979b64b3a71702b0a0a07c1f8c4dd95b2f096a2b532a",
        "b957fb56d8146102a6a4fdd8cb9ba989a807bca3e3721d57734182b57af88c50",
        "53f5ff60e6d99ab2e4be8a5e9e04143c3a00c11ac198f6d03d871785f2a8b583",
        "6f8faf2977f82a067e618ba53ccb15c69a68f48952312ec89af02e6005214c6c",
        "1f6c851ef2f7b73b10c8cacfc375057317738629d490a4ceee5e7069667886fc",
        "217a3ca2198d08f5cfceeb883688efbeff8e6ad36d471a38e122510f537d76a2",
        "070e684d2f3cb06877876c3dab4fdf59b03f0f5c0bfd50e4e31b6189eba8ca86",
        "67cdfec9a513b990b0b19e63780d1ae89b260087598da4a55f4198913551b242",
        "2d42b6f9632ffefb44a89eb02888a76a8c1f3ad109eb88c85861571ce815bca2",
        "26fb79f0e7ca5e74de466533f8e9e7aee94a03ca40cbc690ac3b722f5ed28c4c",
        "197b7242daa1abd7df1534ee009ec9bbd706fc791f135b3bfcdd0dd74de5529d",
        "9206a3a1653b3d1725c221139908b8fc34998b22b99d56316a7b3e81079969ba",
    ],
};

impl KnownHashListSpec {
    /// Returns the known-hash list spec for `network`, if one is pinned.
    ///
    /// Pinned for Mainnet and the default public Testnet. Custom testnets
    /// have their own chain, so they get no pinned list.
    pub fn for_network(network: &Network) -> Option<&'static KnownHashListSpec> {
        match network {
            Network::Mainnet => Some(&MAINNET_KNOWN_HASHES),
            Network::Testnet(params) if params.is_default_testnet() => Some(&TESTNET_KNOWN_HASHES),
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

    /// The number of chunks in the list.
    pub fn chunk_count(&self) -> usize {
        self.chunk_hashes.len()
    }

    /// The chunk index covering `height`, or `None` past the end of the list.
    pub fn chunk_index_for(&self, height: block::Height) -> Option<usize> {
        (height <= self.max_height).then(|| (height.0 / self.chunk_blocks) as usize)
    }

    /// The number of hashes expected in chunk `index`.
    pub fn chunk_len(&self, index: usize) -> u64 {
        let full_chunks = (self.chunk_hashes.len() - 1) as u64;
        if (index as u64) < full_chunks {
            u64::from(self.chunk_blocks)
        } else {
            self.len() - full_chunks * u64::from(self.chunk_blocks)
        }
    }

    /// Returns the chunk index whose pinned SHA-256 content hash is `hash`,
    /// or `None` if no chunk of this list is pinned to that hash.
    ///
    /// This is the serving-side content-address lookup: a `get-object`
    /// request names a chunk by its hash, and the serving node answers with
    /// the chunk's bytes, read from the store or regenerated from state.
    pub fn chunk_index_by_hash(&self, hash: &[u8; 32]) -> Option<usize> {
        let hex_hash = hex::encode(hash);
        self.chunk_hashes
            .iter()
            .position(|&pinned| pinned == hex_hash)
    }

    /// Verifies `bytes` as chunk `index` of this list for `network`, and
    /// returns the parsed chunk on success.
    ///
    /// Chunk bytes are untrusted network data. Verification checks, in order:
    /// the chunk index is in range, the bytes parse as a structurally valid
    /// [v2 chunk](chunk_v2), the declared block count matches the spec's
    /// chunk length, the SHA-256 of the bytes matches the pinned constant,
    /// and — for chunk `0` — the first hash is the network's genesis hash.
    ///
    /// This is the only trust gate between downloaded chunk bytes and the
    /// state's chunk store: bytes that pass here are safe to persist and
    /// serve.
    pub fn verify_chunk_bytes<'a>(
        &self,
        network: &Network,
        index: usize,
        bytes: &'a [u8],
    ) -> Result<chunk_v2::ParsedChunk<'a>, KnownHashError> {
        let Some(&expected_hash) = self.chunk_hashes.get(index) else {
            return Err(KnownHashError::ChunkIndexOutOfRange {
                index,
                chunk_count: self.chunk_count(),
            });
        };

        let parsed = chunk_v2::ParsedChunk::parse(bytes)
            .map_err(|error| KnownHashError::ChunkV2 { index, error })?;

        let expected_blocks = self.chunk_len(index);
        if u64::from(parsed.block_count()) != expected_blocks {
            return Err(KnownHashError::ChunkLength {
                index,
                expected_blocks,
                actual_blocks: u64::from(parsed.block_count()),
                spec_len: self.len(),
            });
        }

        let actual_hash = hex::encode(Sha256::digest(bytes));
        if actual_hash != expected_hash {
            return Err(KnownHashError::ChunkHashMismatch {
                index,
                expected: expected_hash.to_owned(),
                actual: actual_hash,
            });
        }

        if index == 0 {
            let list_genesis = block::Hash(parsed.block_hash(0));
            let network_genesis = network.genesis_hash();
            if list_genesis != network_genesis {
                return Err(KnownHashError::GenesisMismatch {
                    list_genesis,
                    network_genesis,
                });
            }
        }

        Ok(parsed)
    }
}

/// Errors verifying known-hash chunks.
#[derive(Error, Debug)]
pub enum KnownHashError {
    /// The chunk index is past the end of the pinned list.
    #[error("known-hash chunk index {index} is out of range: the list has {chunk_count} chunks")]
    ChunkIndexOutOfRange {
        /// The requested chunk index.
        index: usize,
        /// The number of chunks in the pinned list.
        chunk_count: usize,
    },

    /// The chunk bytes failed their SHA-256 integrity check.
    #[error(
        "known-hash chunk {index} failed its SHA-256 integrity check: \
         expected {expected}, got {actual} — the bytes are corrupt or tampered"
    )]
    ChunkHashMismatch {
        /// The chunk index.
        index: usize,
        /// The pinned hash from the spec constant.
        expected: String,
        /// The hash of the received bytes.
        actual: String,
    },

    /// The chunk's block count does not match the spec.
    #[error(
        "known-hash chunk {index} declares {actual_blocks} blocks, expected \
         {expected_blocks} (the spec pins {spec_len} total hashes; chunk and \
         spec disagree)"
    )]
    ChunkLength {
        /// The chunk index.
        index: usize,
        /// The expected number of blocks in this chunk.
        expected_blocks: u64,
        /// The declared number of blocks in the received chunk.
        actual_blocks: u64,
        /// The total number of hashes pinned by the spec.
        spec_len: u64,
    },

    /// The chunk bytes failed structural validation.
    #[error("known-hash chunk {index} failed to parse: {error}")]
    ChunkV2 {
        /// The chunk index.
        index: usize,
        /// The structural parse failure.
        #[source]
        error: chunk_v2::ChunkV2Error,
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
