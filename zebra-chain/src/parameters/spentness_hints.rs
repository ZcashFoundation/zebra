//! Spentness-hint artifacts for hinted checkpoint synchronization.
//!
//! A spentness-hint artifact is a bitmap with one bit per transparent output
//! created at or below a release-pinned final checkpoint, in canonical
//! creation order: block height ascending, then transaction index within the
//! block, then output index within the transaction. Genesis outputs are
//! included in the count. A set bit means the output is spent at or below the
//! final checkpoint; a clear bit means it is still unspent there.
//!
//! The artifact is trusted only through [`MaxCheckpoint`]: the release pins
//! the final checkpoint's height and block hash together with the SHA-256
//! hash of the whole serialized artifact, and a downloaded artifact is used
//! only if its [`SpentnessHints::artifact_hash`] matches the pinned
//! `spentness_hash`.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::{
    block,
    parameters::{Magic, Network},
};

#[cfg(test)]
mod tests;

/// The artifact magic bytes: the fixed prefix of every serialized
/// spentness-hint artifact.
pub const SPENTNESS_HINTS_MAGIC: [u8; 4] = *b"ZSH1";

/// The only serialization version this implementation reads and writes.
pub const SPENTNESS_HINTS_VERSION: u8 = 1;

/// The length of the fixed artifact header:
/// magic (4) + version (1) + network magic (4) + max checkpoint height (4) +
/// output count (8).
const HEADER_LEN: usize = 21;

/// An error rejecting a malformed or inconsistent spentness-hint artifact.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SpentnessHintsError {
    /// The artifact is shorter than its declared contents.
    #[error("truncated artifact: got {actual} bytes, expected {expected}")]
    Truncated {
        /// The serialized length the header declares.
        expected: u64,
        /// The length actually supplied.
        actual: u64,
    },

    /// The artifact is longer than its declared contents.
    #[error("trailing bytes after the bitmap: got {actual} bytes, expected {expected}")]
    TrailingBytes {
        /// The serialized length the header declares.
        expected: u64,
        /// The length actually supplied.
        actual: u64,
    },

    /// The artifact does not start with [`SPENTNESS_HINTS_MAGIC`].
    #[error("unexpected artifact magic {0:02x?}")]
    WrongMagic([u8; 4]),

    /// The artifact uses a serialization version this implementation does not
    /// support.
    #[error("unsupported artifact version {0}")]
    UnsupportedVersion(u8),

    /// The artifact was generated for a different network.
    #[error("artifact network magic {artifact:?} does not match the expected {expected:?}")]
    NetworkMismatch {
        /// The magic of the network the caller is syncing.
        expected: Magic,
        /// The magic the artifact declares.
        artifact: Magic,
    },

    /// The declared max checkpoint height is not a valid block height.
    #[error("max checkpoint height {0} exceeds the maximum block height")]
    HeightOutOfRange(u32),

    /// Unused bits in the final bitmap byte are not zero, so the serialization
    /// is not canonical.
    #[error("padding bits past the declared output count are not zero")]
    NonZeroPadding,
}

/// An immutable spentness-hint bitmap over global transparent-output ordinals.
///
/// The `n`-th bit (bytes in order, least significant bit first within each
/// byte) is the hint for the `n`-th transparent output ever created on the
/// network, counting from the genesis block in canonical order: height
/// ascending, then transaction index, then output index.
#[derive(Clone, Eq, PartialEq)]
pub struct SpentnessHints {
    /// The exact serialized artifact, header included.
    ///
    /// Kept whole so [`Self::artifact_hash`] is the hash of the bytes as
    /// distributed, and the bitmap is a zero-copy slice of it.
    bytes: Vec<u8>,

    /// The final checkpoint height the bitmap covers.
    max_height: block::Height,

    /// The number of hint bits: the number of transparent outputs created at
    /// or below [`Self::max_height`].
    output_count: u64,

    /// SHA-256 over [`Self::bytes`], computed once at construction.
    artifact_hash: [u8; 32],
}

impl fmt::Debug for SpentnessHints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpentnessHints")
            .field("max_height", &self.max_height)
            .field("output_count", &self.output_count)
            .field("artifact_hash", &hex::encode(self.artifact_hash))
            .finish()
    }
}

/// Returns the `N` header bytes starting at `start`.
///
/// # Panics
///
/// If `start + N` is past the end of `bytes`. Callers read fixed offsets
/// within the first [`HEADER_LEN`] bytes after checking the length.
fn header_field<const N: usize>(bytes: &[u8], start: usize) -> [u8; N] {
    bytes[start..start + N]
        .try_into()
        .expect("an indexable range of length N converts to [u8; N]")
}

impl SpentnessHints {
    /// Parses a serialized artifact for `network`, rejecting any framing
    /// inconsistency: wrong magic or version, a different network, an invalid
    /// height, a bitmap length that does not match the declared output count,
    /// non-zero padding bits, and any missing or trailing bytes.
    pub fn from_bytes(network: &Network, bytes: Vec<u8>) -> Result<Self, SpentnessHintsError> {
        if bytes.len() < HEADER_LEN {
            return Err(SpentnessHintsError::Truncated {
                expected: HEADER_LEN as u64,
                actual: bytes.len() as u64,
            });
        }

        let magic: [u8; 4] = header_field(&bytes, 0);
        if magic != SPENTNESS_HINTS_MAGIC {
            return Err(SpentnessHintsError::WrongMagic(magic));
        }

        let version = bytes[4];
        if version != SPENTNESS_HINTS_VERSION {
            return Err(SpentnessHintsError::UnsupportedVersion(version));
        }

        let network_magic = Magic(header_field(&bytes, 5));
        if network_magic != network.magic() {
            return Err(SpentnessHintsError::NetworkMismatch {
                expected: network.magic(),
                artifact: network_magic,
            });
        }

        let raw_height = u32::from_le_bytes(header_field(&bytes, 9));
        let max_height = block::Height::try_from(raw_height)
            .map_err(|_invalid_height_error| SpentnessHintsError::HeightOutOfRange(raw_height))?;

        let output_count = u64::from_le_bytes(header_field(&bytes, 13));

        let bitmap_len = output_count.div_ceil(8);
        // Can't overflow: bitmap_len is at most u64::MAX / 8 + 1.
        let expected_len = HEADER_LEN as u64 + bitmap_len;
        let actual_len = bytes.len() as u64;

        if actual_len < expected_len {
            return Err(SpentnessHintsError::Truncated {
                expected: expected_len,
                actual: actual_len,
            });
        }
        if actual_len > expected_len {
            return Err(SpentnessHintsError::TrailingBytes {
                expected: expected_len,
                actual: actual_len,
            });
        }

        // Safety: the length-equality checks above bound `bitmap_len` by the
        // input length, and `output_count <= bitmap_len * 8` follows from
        // `bitmap_len = output_count.div_ceil(8)`, so neither operation can
        // overflow or wrap for inputs that reach this point.
        let padding_bits = bitmap_len
            .checked_mul(8)
            .and_then(|bits| bits.checked_sub(output_count))
            .expect("bitmap length is derived from the output count by div_ceil");
        if padding_bits > 0 {
            let last = bytes[bytes.len() - 1];
            // Safe cast: padding_bits < 8 by construction of div_ceil.
            if last >> (8 - padding_bits as u32) != 0 {
                return Err(SpentnessHintsError::NonZeroPadding);
            }
        }

        let artifact_hash = Sha256::digest(&bytes).into();

        Ok(Self {
            bytes,
            max_height,
            output_count,
            artifact_hash,
        })
    }

    /// Builds a canonical artifact from an iterator of hint bits in canonical
    /// output order, for the artifact-generation pipeline.
    ///
    /// The result round-trips through [`Self::from_bytes`] for the same
    /// `network`.
    pub fn from_bits(
        network: &Network,
        max_height: block::Height,
        bits: impl IntoIterator<Item = bool>,
    ) -> Self {
        let mut bytes = Vec::with_capacity(HEADER_LEN);
        bytes.extend_from_slice(&SPENTNESS_HINTS_MAGIC);
        bytes.push(SPENTNESS_HINTS_VERSION);
        bytes.extend_from_slice(&network.magic().0);
        bytes.extend_from_slice(&max_height.0.to_le_bytes());
        // Filled in below, once the bits have been counted.
        bytes.extend_from_slice(&[0; 8]);

        let mut output_count: u64 = 0;
        let mut current = 0u8;
        for bit in bits {
            // Safe cast: output_count % 8 is less than 8.
            let bit_index = (output_count % 8) as u32;
            if bit {
                current |= 1 << bit_index;
            }
            output_count = output_count
                .checked_add(1)
                .expect("an in-memory bit iterator yields far fewer than u64::MAX items");
            if bit_index == 7 {
                bytes.push(current);
                current = 0;
            }
        }
        if !output_count.is_multiple_of(8) {
            bytes.push(current);
        }

        bytes[13..21].copy_from_slice(&output_count.to_le_bytes());

        let artifact_hash = Sha256::digest(&bytes).into();

        Self {
            bytes,
            max_height,
            output_count,
            artifact_hash,
        }
    }

    /// Returns the hint for the output with the given global ordinal, or
    /// `None` if the ordinal is at or past [`Self::output_count`] (an output
    /// created above the final checkpoint has no hint).
    pub fn is_spent(&self, ordinal: u64) -> Option<bool> {
        if ordinal >= self.output_count {
            return None;
        }

        // Safe cast: ordinal / 8 is less than the bitmap length, which fits in
        // usize because the bitmap is an in-memory slice.
        let byte = self.bytes[HEADER_LEN + (ordinal / 8) as usize];
        // Safe cast: ordinal % 8 is less than 8.
        let bit_index = (ordinal % 8) as u32;

        Some((byte >> bit_index) & 1 == 1)
    }

    /// The number of hint bits: the number of transparent outputs created at
    /// or below [`Self::max_height`], genesis outputs included.
    pub fn output_count(&self) -> u64 {
        self.output_count
    }

    /// The final checkpoint height the bitmap covers.
    pub fn max_height(&self) -> block::Height {
        self.max_height
    }

    /// SHA-256 over the exact serialized artifact bytes.
    ///
    /// This is the value release tooling pins as
    /// [`MaxCheckpoint::spentness_hash`], and the artifact's content address.
    pub fn artifact_hash(&self) -> [u8; 32] {
        self.artifact_hash
    }

    /// The exact serialized artifact bytes, header included.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The release-pinned trust root for hinted synchronization on one network.
///
/// A spentness-hint artifact influences state only after its
/// [`SpentnessHints::artifact_hash`] matches `spentness_hash`, so the bitmap
/// carries exactly the trust of the pinned checkpoint `hash`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaxCheckpoint {
    /// The final checkpoint height.
    pub height: block::Height,

    /// The hash of the final checkpoint block.
    pub hash: block::Hash,

    /// SHA-256 of the whole serialized spentness-hint artifact for `height`.
    pub spentness_hash: [u8; 32],
}

/// The pinned Mainnet trust root.
///
/// `None` until release tooling cuts the first Mainnet artifact and fills in
/// the pinned values; hinted synchronization stays disabled while it is
/// absent.
static MAINNET_MAX_CHECKPOINT: Option<MaxCheckpoint> = None;

/// The pinned default-Testnet trust root.
///
/// `None` until release tooling cuts the first Testnet artifact and fills in
/// the pinned values; hinted synchronization stays disabled while it is
/// absent.
static TESTNET_MAX_CHECKPOINT: Option<MaxCheckpoint> = None;

/// Returns the release-pinned [`MaxCheckpoint`] for `network`, or `None` when
/// the network has no pinned artifact (custom testnets and Regtest never do,
/// and Mainnet and the default Testnet have none until release tooling fills
/// in the constants above).
pub fn max_checkpoint(network: &Network) -> Option<&'static MaxCheckpoint> {
    match network {
        Network::Mainnet => MAINNET_MAX_CHECKPOINT.as_ref(),
        Network::Testnet(params) if params.is_default_testnet() => TESTNET_MAX_CHECKPOINT.as_ref(),
        Network::Testnet(_) => None,
    }
}
