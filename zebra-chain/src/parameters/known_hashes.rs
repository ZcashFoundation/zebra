//! Pinned known-hash lists (zips#1346, "known-hash list"; design §5).
//!
//! A list covers heights `0..=max_height` and is split into chunks of
//! [`CHUNK_HEIGHTS`] heights. Chunk `k` is the canonical `get-hashes`
//! response body for `(start_height = CHUNK_HEIGHTS·k, count = chunk_len(k))`,
//! as written by
//! [`write_sync_hashes_body`](crate::block::sync_metadata::write_sync_hashes_body),
//! and its SHA-256 is pinned in this crate. Chunks are fetched from peers
//! with `get-hashes`, re-encoded canonically, and verified here.
//!
//! Mainnet and the default Testnet have no pinned list yet.

use std::borrow::Cow;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    block::{self, sync_metadata::write_sync_hashes_body, SyncHashEntry},
    parameters::Network,
};

#[cfg(test)]
mod tests;

/// The number of heights in every chunk except the last (design decision D5).
pub const CHUNK_HEIGHTS: u32 = 50_000;

/// A pinned known-hash list for one network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownHashListSpec {
    /// The highest height the list covers. At most the network's final
    /// checkpoint height: [`KnownHashListSpec::from_config`] refuses a higher
    /// one, and a unit test asserts this for the pinned lists.
    pub max_height: block::Height,

    /// The heights per chunk, except the last: [`CHUNK_HEIGHTS`] for every
    /// pinned list.
    pub chunk_heights: u32,

    /// SHA-256 of each chunk's canonical bytes, in chunk order. The last
    /// chunk ends at `max_height`.
    pub chunk_hashes: Cow<'static, [[u8; 32]]>,
}

impl KnownHashListSpec {
    /// The pinned list for `network`: Mainnet and the default Testnet only.
    ///
    /// Returns `None` until their lists are pinned, and always for Regtest
    /// and custom testnets, which take their pins from config through
    /// [`KnownHashListSpec::from_config`].
    pub fn for_network(network: &Network) -> Option<KnownHashListSpec> {
        match network {
            // The Mainnet and default Testnet pins go here.
            Network::Mainnet => None,
            Network::Testnet(params) if params.is_default_testnet() => None,
            Network::Testnet(_) => None,
        }
    }

    /// A list for a custom testnet or Regtest, from pins its caller
    /// configured.
    ///
    /// # Errors
    ///
    /// [`KnownHashError::PinsNotConfigurable`] on Mainnet and the default
    /// Testnet; [`KnownHashError::AboveFinalCheckpoint`] if `max_height` is
    /// above the network's final checkpoint;
    /// [`KnownHashError::InconsistentSpec`] if the chunk count does not cover
    /// exactly `0..=max_height`.
    pub fn from_config(
        network: &Network,
        max_height: block::Height,
        chunk_hashes: Vec<[u8; 32]>,
    ) -> Result<KnownHashListSpec, KnownHashError> {
        if matches!(network, Network::Mainnet) || network.is_default_testnet() {
            return Err(KnownHashError::PinsNotConfigurable {
                network: network.to_string(),
            });
        }

        // Checkpoint sync, the list's only user, runs below the final checkpoint, and
        // responders only serve heights well below their tips, so a list that reaches above
        // the final checkpoint might never be completed.
        let final_checkpoint = network.checkpoint_list().max_height();
        if max_height > final_checkpoint {
            return Err(KnownHashError::AboveFinalCheckpoint {
                max_height,
                final_checkpoint,
            });
        }

        let spec = KnownHashListSpec {
            max_height,
            chunk_heights: CHUNK_HEIGHTS,
            chunk_hashes: Cow::Owned(chunk_hashes),
        };
        spec.check_consistent()?;

        Ok(spec)
    }

    /// Checks that the chunk count covers exactly `0..=max_height`, at
    /// `chunk_heights` heights per chunk.
    fn check_consistent(&self) -> Result<(), KnownHashError> {
        if self.chunk_heights == 0 {
            return Err(KnownHashError::InconsistentSpec(
                "chunks must cover at least one height",
            ));
        }

        if self.chunk_hashes.is_empty() {
            return Err(KnownHashError::InconsistentSpec(
                "a list covers at least the genesis block, so it has at least one chunk",
            ));
        }

        let expected = self.len().div_ceil(u64::from(self.chunk_heights));
        // A `usize` always fits in a `u64` on supported platforms.
        if self.chunk_hashes.len() as u64 != expected {
            return Err(KnownHashError::InconsistentSpec(
                "the chunk count does not cover exactly 0..=max_height",
            ));
        }

        Ok(())
    }

    /// The number of heights in the list, `max_height + 1`.
    pub fn len(&self) -> u64 {
        u64::from(self.max_height.0) + 1
    }

    /// Always false for a valid spec.
    pub fn is_empty(&self) -> bool {
        self.chunk_hashes.is_empty()
    }

    /// The number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunk_hashes.len()
    }

    /// The chunk covering `height`, or `None` above `max_height`.
    pub fn chunk_index_for(&self, height: block::Height) -> Option<usize> {
        if height > self.max_height || self.chunk_heights == 0 {
            return None;
        }

        // A `u32` quotient always fits in a `usize` on supported platforms.
        let index = (height.0 / self.chunk_heights) as usize;

        (index < self.chunk_count()).then_some(index)
    }

    /// The first height of chunk `index`, or `None` past the last chunk.
    pub fn chunk_start(&self, index: usize) -> Option<block::Height> {
        if index >= self.chunk_count() {
            return None;
        }

        let start = u32::try_from(index).ok()?.checked_mul(self.chunk_heights)?;

        (start <= self.max_height.0).then_some(block::Height(start))
    }

    /// The number of heights in chunk `index` (`chunk_heights`, except the
    /// last), or `None` past the last chunk.
    pub fn chunk_len(&self, index: usize) -> Option<u32> {
        let start = self.chunk_start(index)?;

        let remaining = self.len() - u64::from(start.0);
        let len = remaining.min(u64::from(self.chunk_heights));

        // `len` is at most `chunk_heights`, a `u32`.
        Some(len as u32)
    }

    /// Verifies the reassembled entries of chunk `index`.
    ///
    /// Re-encodes `entries` canonically, then checks, in order: the index is
    /// in range, `entries.len() == chunk_len(index)`, the SHA-256 of the bytes
    /// matches the pin, and for chunk 0 that the first hash is `network`'s
    /// genesis hash.
    ///
    /// A mismatch is never attributed to a peer (zips#1344 P:1979-1982): the
    /// caller discards the data and retries elsewhere, with no penalty.
    pub fn verify_chunk(
        &self,
        network: &Network,
        index: usize,
        entries: &[SyncHashEntry],
    ) -> Result<(), KnownHashError> {
        let (Some(expected_pin), Some(expected_len)) =
            (self.chunk_hashes.get(index), self.chunk_len(index))
        else {
            return Err(KnownHashError::ChunkIndexOutOfRange {
                index,
                chunk_count: self.chunk_count(),
            });
        };

        // The length is checked before anything is encoded, so a short or
        // overlong chunk costs no hashing.
        //
        // `u32` always fits in a `usize` on supported platforms.
        if entries.len() != expected_len as usize {
            return Err(KnownHashError::ChunkLength {
                index,
                expected: expected_len,
                actual: entries.len(),
            });
        }

        if chunk_pin(entries) != *expected_pin {
            return Err(KnownHashError::ChunkHashMismatch { index });
        }

        if index == 0 {
            let list_genesis = entries
                .first()
                .expect("chunk 0 has at least one entry, because every list covers height 0")
                .hash;
            let network_genesis = network.genesis_hash();

            if list_genesis != network_genesis {
                return Err(KnownHashError::GenesisMismatch {
                    list_genesis,
                    network_genesis,
                });
            }
        }

        Ok(())
    }
}

/// The pin of a chunk: the SHA-256 of its canonical `get-hashes` body,
/// as written by
/// [`write_sync_hashes_body`](crate::block::sync_metadata::write_sync_hashes_body).
///
/// The body is streamed into the hasher, so no copy of the chunk bytes is
/// held. This is the single definition shared by [`KnownHashListSpec::verify_chunk`]
/// and the `known-hashes` pinning tool in `zebra-utils`.
pub fn chunk_pin(entries: &[SyncHashEntry]) -> [u8; 32] {
    let mut hasher = Sha256::new();

    write_sync_hashes_body(entries, &mut hasher)
        .expect("writing to a hasher never fails, because it is an in-memory sink");

    hasher.finalize().into()
}

/// Errors building or verifying a known-hash list.
#[derive(Error, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum KnownHashError {
    /// The chunk index is past the end of the list.
    #[error("known-hash chunk {index} is out of range: the list has {chunk_count} chunks")]
    ChunkIndexOutOfRange {
        /// The chunk index.
        index: usize,
        /// The number of chunks in the list.
        chunk_count: usize,
    },

    /// The chunk has the wrong number of entries.
    #[error("known-hash chunk {index} has {actual} entries, expected {expected}")]
    ChunkLength {
        /// The chunk index.
        index: usize,
        /// The pinned length.
        expected: u32,
        /// The received length.
        actual: usize,
    },

    /// The chunk's canonical bytes do not hash to the pin.
    #[error("known-hash chunk {index} does not match its pin")]
    ChunkHashMismatch {
        /// The chunk index.
        index: usize,
    },

    /// Chunk 0 does not start at the network's genesis block.
    #[error(
        "known-hash list genesis {list_genesis:?} is not the network genesis {network_genesis:?}"
    )]
    GenesisMismatch {
        /// The first hash in chunk 0.
        list_genesis: block::Hash,
        /// The network's genesis hash.
        network_genesis: block::Hash,
    },

    /// Pins can only be configured on Regtest and custom testnets.
    #[error("known-hash pins are compiled in for {network}, and cannot be configured")]
    PinsNotConfigurable {
        /// The network's name.
        network: String,
    },

    /// The list's `max_height` is above the network's final checkpoint.
    #[error(
        "known-hash list max height {max_height:?} is above the final checkpoint {final_checkpoint:?}"
    )]
    AboveFinalCheckpoint {
        /// The list's highest height.
        max_height: block::Height,
        /// The network's final checkpoint height.
        final_checkpoint: block::Height,
    },

    /// The chunk count does not cover exactly `0..=max_height`.
    #[error("known-hash list spec is inconsistent: {0}")]
    InconsistentSpec(&'static str),
}
