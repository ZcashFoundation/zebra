//! Spentness-hint artifacts (SwiftSync; zips#1346 S:304-390).
//!
//! An artifact is one bit per transparent output created at heights
//! `0..=terminal_height`, in canonical order (block height, transaction
//! index, output index; genesis outputs included): `1` if the output is
//! still unspent at the terminal checkpoint, `0` if it is spent by then.
//!
//! Layout (Zakura's `ZKSHINT\0` format, version 1), integers little-endian:
//!
//! | offset | len | field |
//! |---|---|---|
//! | 0  | 8  | magic `ZKSHINT\0` |
//! | 8  | 2  | `format_version` = [`FORMAT_VERSION`] |
//! | 10 | 32 | `chain_identity`: the genesis block hash, serialized byte order |
//! | 42 | 4  | `terminal_height` |
//! | 46 | 32 | `terminal_hash`, serialized byte order |
//! | 78 | 8  | `output_count` |
//! | 86 | ⌈output_count / 8⌉ | bits, LSB-first within each byte; unused high bits of the last byte are zero |
//!
//! The artifact is trusted only through a pinned [`MaxCheckpoint`]: its
//! SHA-256 must equal `spentness_hash`, and its header must name the pin's
//! height and hash and the network's genesis.
//!
//! The genesis coinbase output is never in the UTXO set (Zebra's finalized
//! writer skips genesis UTXOs), so its bit, ordinal 0, must be `0`.

use std::{mem::size_of, sync::Arc};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{block, parameters::Network, state_snapshot::ObjectHash};

#[cfg(test)]
mod tests;

const HASH_LEN: usize = 32;
const BITS_PER_BYTE: u64 = 8;

/// The artifact magic: the first 8 bytes of every artifact.
const MAGIC: &[u8; 8] = b"ZKSHINT\0";

const VERSION_OFFSET: usize = MAGIC.len();
const CHAIN_IDENTITY_OFFSET: usize = VERSION_OFFSET + size_of::<u16>();
const TERMINAL_HEIGHT_OFFSET: usize = CHAIN_IDENTITY_OFFSET + HASH_LEN;
const TERMINAL_HASH_OFFSET: usize = TERMINAL_HEIGHT_OFFSET + size_of::<u32>();
const OUTPUT_COUNT_OFFSET: usize = TERMINAL_HASH_OFFSET + HASH_LEN;

/// The artifact header length, in bytes.
pub const HEADER_LEN: usize = OUTPUT_COUNT_OFFSET + size_of::<u64>();

static_assertions::const_assert_eq!(HEADER_LEN, 86);

/// The largest artifact accepted, checked before any allocation (512 MiB).
pub const MAX_ARTIFACT_LEN: u64 = 512 * 1024 * 1024;

/// The artifact format version this node reads and writes.
pub const FORMAT_VERSION: u16 = 1;

/// The pinned trust root of a network's spentness hints: the final
/// checkpoint and the hash of the artifact generated at it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaxCheckpoint {
    /// The terminal checkpoint height, `H`. Hints never apply above it.
    pub height: block::Height,

    /// The hash of the block at `height`; it must be in the network's
    /// checkpoint list.
    pub hash: block::Hash,

    /// SHA-256 of the complete serialized artifact for `height`.
    pub spentness_hash: [u8; 32],
}

impl MaxCheckpoint {
    /// The pinned trust root for `network`, or `None` if it has no hints
    /// (every network until a release pins one; Regtest and custom testnets
    /// always).
    pub fn for_network(network: &Network) -> Option<MaxCheckpoint> {
        match network {
            Network::Mainnet => MAINNET_MAX_CHECKPOINT,
            Network::Testnet(params) if params.is_default_testnet() => TESTNET_MAX_CHECKPOINT,
            Network::Testnet(_) => None,
        }
    }

    /// The pin for `network` whose artifact has SHA-256 `spentness_hash`: the
    /// current pin, or a retired one from an earlier release that was not
    /// revoked, or `None` if this release pins no such artifact.
    ///
    /// A hinted sync that an earlier release started must resume with the
    /// artifact it started with, even after this release pins a newer one.
    pub fn for_commitment(network: &Network, spentness_hash: [u8; 32]) -> Option<MaxCheckpoint> {
        let retired = match network {
            Network::Mainnet => RETIRED_MAINNET_MAX_CHECKPOINTS,
            Network::Testnet(params) if params.is_default_testnet() => {
                RETIRED_TESTNET_MAX_CHECKPOINTS
            }
            Network::Testnet(_) => &[],
        };
        find_pin(Self::for_network(network), retired, spentness_hash)
    }

    /// The `get-object` content address of the pinned artifact.
    pub fn object_hash(&self) -> ObjectHash {
        ObjectHash::from(self.spentness_hash)
    }
}

/// The pin among `current` and `retired` whose artifact has SHA-256 `spentness_hash`.
fn find_pin(
    current: Option<MaxCheckpoint>,
    retired: &[MaxCheckpoint],
    spentness_hash: [u8; 32],
) -> Option<MaxCheckpoint> {
    current
        .into_iter()
        .chain(retired.iter().copied())
        .find(|pin| pin.spentness_hash == spentness_hash)
}

/// The pinned Mainnet trust root.
///
/// `None` until a release generates the first Mainnet artifact and pins it;
/// hinted synchronization stays disabled while it is absent.
const MAINNET_MAX_CHECKPOINT: Option<MaxCheckpoint> = None;

/// The pinned default-Testnet trust root.
///
/// `None` until a release generates the first Testnet artifact and pins it;
/// hinted synchronization stays disabled while it is absent.
const TESTNET_MAX_CHECKPOINT: Option<MaxCheckpoint> = None;

/// Earlier Mainnet pins, which nodes still use to resume a hinted sync that
/// an earlier release started (see [`MaxCheckpoint::for_commitment`]).
///
/// When a release replaces [`MAINNET_MAX_CHECKPOINT`], move the old pin here,
/// unless it is being revoked: a revoked pin is left out, so its artifact is
/// never loaded again, and a node part way through a hinted sync with it stops
/// at startup instead.
const RETIRED_MAINNET_MAX_CHECKPOINTS: &[MaxCheckpoint] = &[];

/// Earlier default-Testnet pins; see [`RETIRED_MAINNET_MAX_CHECKPOINTS`].
const RETIRED_TESTNET_MAX_CHECKPOINTS: &[MaxCheckpoint] = &[];

/// The fixed header of an artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactHeader {
    /// The genesis block hash of the artifact's chain.
    pub chain_identity: block::Hash,

    /// The terminal checkpoint height `H`.
    pub terminal_height: block::Height,

    /// The block hash at `terminal_height`.
    pub terminal_hash: block::Hash,

    /// The number of transparent outputs created at heights `0..=H`: the
    /// number of bits.
    pub output_count: u64,
}

impl ArtifactHeader {
    /// Parses the fixed header at the start of `bytes`.
    fn parse(bytes: &[u8]) -> Result<ArtifactHeader, SpentnessHintsError> {
        let header: &[u8; HEADER_LEN] = bytes
            .get(..HEADER_LEN)
            .and_then(|header| header.try_into().ok())
            .ok_or(SpentnessHintsError::Format("truncated artifact header"))?;

        if &header[..MAGIC.len()] != MAGIC {
            return Err(SpentnessHintsError::Format("unknown magic"));
        }
        if u16::from_le_bytes(header_field(header, VERSION_OFFSET)) != FORMAT_VERSION {
            return Err(SpentnessHintsError::Format("unknown format version"));
        }

        let terminal_height = block::Height::try_from(u32::from_le_bytes(header_field(
            header,
            TERMINAL_HEIGHT_OFFSET,
        )))
        .map_err(|_| SpentnessHintsError::Format("terminal height is out of range"))?;

        Ok(ArtifactHeader {
            chain_identity: block::Hash(header_field(header, CHAIN_IDENTITY_OFFSET)),
            terminal_height,
            terminal_hash: block::Hash(header_field(header, TERMINAL_HASH_OFFSET)),
            output_count: u64::from_le_bytes(header_field(header, OUTPUT_COUNT_OFFSET)),
        })
    }

    /// Serializes the header.
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut bytes = [0; HEADER_LEN];
        bytes[..VERSION_OFFSET].copy_from_slice(MAGIC);
        bytes[VERSION_OFFSET..CHAIN_IDENTITY_OFFSET].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[CHAIN_IDENTITY_OFFSET..TERMINAL_HEIGHT_OFFSET]
            .copy_from_slice(&self.chain_identity.0);
        bytes[TERMINAL_HEIGHT_OFFSET..TERMINAL_HASH_OFFSET]
            .copy_from_slice(&self.terminal_height.0.to_le_bytes());
        bytes[TERMINAL_HASH_OFFSET..OUTPUT_COUNT_OFFSET].copy_from_slice(&self.terminal_hash.0);
        bytes[OUTPUT_COUNT_OFFSET..HEADER_LEN].copy_from_slice(&self.output_count.to_le_bytes());
        bytes
    }
}

/// Returns the `N` header bytes at `offset`.
fn header_field<const N: usize>(header: &[u8; HEADER_LEN], offset: usize) -> [u8; N] {
    header[offset..offset + N]
        .try_into()
        .expect("every header field offset is a constant inside the fixed-size header")
}

/// Rejects nonzero unused bits and a retained genesis output.
fn check_bits(bits: &[u8], output_count: u64) -> Result<(), SpentnessHintsError> {
    let used_bits = output_count % BITS_PER_BYTE;
    if used_bits != 0 && bits.last().is_some_and(|last| last >> used_bits != 0) {
        return Err(SpentnessHintsError::Format("nonzero unused bits"));
    }
    if bits.first().is_some_and(|first| first & 1 != 0) {
        return Err(SpentnessHintsError::Format(
            "genesis output must not be retained",
        ));
    }
    Ok(())
}

/// The byte index and bit mask of `ordinal` in the full artifact.
fn bit_position(ordinal: u64) -> Result<(usize, u8), SpentnessHintsError> {
    let byte = usize::try_from(ordinal / BITS_PER_BYTE)
        .ok()
        .and_then(|byte| byte.checked_add(HEADER_LEN))
        .ok_or(SpentnessHintsError::Ordinal)?;
    Ok((byte, 1 << (ordinal % BITS_PER_BYTE)))
}

/// The exact artifact length for `output_count` outputs:
/// `HEADER_LEN + ⌈output_count / 8⌉`.
///
/// # Errors
///
/// [`SpentnessHintsError::Format`] if the length would exceed
/// [`MAX_ARTIFACT_LEN`].
pub fn artifact_len(output_count: u64) -> Result<u64, SpentnessHintsError> {
    let header_len = u64::try_from(HEADER_LEN)
        .map_err(|_| SpentnessHintsError::Format("artifact header is too large"))?;
    output_count
        .div_ceil(BITS_PER_BYTE)
        .checked_add(header_len)
        .filter(|len| *len <= MAX_ARTIFACT_LEN)
        .ok_or(SpentnessHintsError::Format(
            "output count exceeds the artifact limit",
        ))
}

/// An artifact whose structure is valid, but whose bytes are not yet
/// authenticated against a pin.
#[derive(Clone, Debug)]
pub struct ParsedArtifact {
    bytes: Arc<[u8]>,
    header: ArtifactHeader,
}

impl ParsedArtifact {
    /// Parses a complete artifact.
    ///
    /// Checks the magic, the version, `bytes.len() == artifact_len(output_count)`,
    /// and that the unused high bits of the last byte are zero. Hashes
    /// nothing.
    pub fn parse(bytes: Arc<[u8]>) -> Result<ParsedArtifact, SpentnessHintsError> {
        let header = ArtifactHeader::parse(&bytes)?;

        let len = u64::try_from(bytes.len())
            .map_err(|_| SpentnessHintsError::Format("artifact length exceeds u64"))?;
        if len != artifact_len(header.output_count)? {
            return Err(SpentnessHintsError::Format(
                "artifact length does not match its output count",
            ));
        }

        check_bits(&bytes[HEADER_LEN..], header.output_count)?;

        Ok(ParsedArtifact { bytes, header })
    }

    /// The parsed header.
    pub fn header(&self) -> &ArtifactHeader {
        &self.header
    }

    /// Authenticates the artifact against `pin` for `network`: its SHA-256
    /// must equal `pin.spentness_hash`, `terminal_height`/`terminal_hash` must
    /// equal the pin's, and `chain_identity` must be `network`'s genesis hash.
    ///
    /// The bit count against the known-hash `txouts` sum is the caller's
    /// check (`zebrad`'s hint acquisition), because the entries are not
    /// available here.
    pub fn verify(
        self,
        network: &Network,
        pin: &MaxCheckpoint,
    ) -> Result<VerifiedArtifact, SpentnessHintsError> {
        if self.header.chain_identity != network.genesis_hash() {
            return Err(SpentnessHintsError::PinMismatch(
                "chain identity is not the network's genesis hash",
            ));
        }
        if self.header.terminal_height != pin.height || self.header.terminal_hash != pin.hash {
            return Err(SpentnessHintsError::PinMismatch(
                "terminal block is not the pinned checkpoint",
            ));
        }

        let digest: [u8; 32] = Sha256::digest(&self.bytes).into();
        if digest != pin.spentness_hash {
            return Err(SpentnessHintsError::PinMismatch(
                "artifact hash is not the pinned spentness hash",
            ));
        }

        Ok(VerifiedArtifact {
            bytes: self.bytes,
            header: self.header,
            pin: *pin,
        })
    }
}

/// An artifact authenticated against its network's [`MaxCheckpoint`]: it
/// carries exactly the trust of the pinned checkpoint.
///
/// Cheap to clone: the bytes are shared.
#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    bytes: Arc<[u8]>,
    header: ArtifactHeader,
    pin: MaxCheckpoint,
}

impl VerifiedArtifact {
    /// The authenticated header.
    pub fn header(&self) -> &ArtifactHeader {
        &self.header
    }

    /// The pin the artifact was verified against.
    pub fn pin(&self) -> &MaxCheckpoint {
        &self.pin
    }

    /// The complete serialized artifact, header included: the `get-object`
    /// payload this node serves.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// Whether the output with canonical ordinal `ordinal` survives to `H`
    /// (`true`: write it as a UTXO; `false`: it is spent at or below `H`).
    ///
    /// # Errors
    ///
    /// [`SpentnessHintsError::Ordinal`] if `ordinal >= output_count`.
    pub fn retains(&self, ordinal: u64) -> Result<bool, SpentnessHintsError> {
        if ordinal >= self.header.output_count {
            return Err(SpentnessHintsError::Ordinal);
        }
        let (byte, mask) = bit_position(ordinal)?;
        Ok(self.bytes[byte] & mask != 0)
    }
}

/// Builds an artifact from survivor bits pushed in canonical order (for
/// `zebra-utils`'s `spentness-hints` generation tool).
#[derive(Debug)]
pub struct ArtifactEncoder {
    bytes: Vec<u8>,
    output_count: u64,
    pushed: u64,
}

impl ArtifactEncoder {
    /// Starts an artifact for `output_count` outputs ending at
    /// `terminal_height`/`terminal_hash` on the chain with genesis
    /// `chain_identity`.
    ///
    /// # Errors
    ///
    /// [`SpentnessHintsError::Format`] if `output_count` outputs would exceed
    /// [`MAX_ARTIFACT_LEN`].
    pub fn new(
        chain_identity: block::Hash,
        terminal_height: block::Height,
        terminal_hash: block::Hash,
        output_count: u64,
    ) -> Result<ArtifactEncoder, SpentnessHintsError> {
        let len = usize::try_from(artifact_len(output_count)?)
            .map_err(|_| SpentnessHintsError::Format("artifact cannot fit in memory"))?;

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| SpentnessHintsError::Format("artifact allocation failed"))?;
        bytes.extend_from_slice(
            &ArtifactHeader {
                chain_identity,
                terminal_height,
                terminal_hash,
                output_count,
            }
            .encode(),
        );
        bytes.resize(len, 0);

        Ok(ArtifactEncoder {
            bytes,
            output_count,
            pushed: 0,
        })
    }

    /// Appends the next output's bit.
    ///
    /// # Errors
    ///
    /// [`SpentnessHintsError::Ordinal`] after `output_count` bits, and
    /// [`SpentnessHintsError::Format`] if the genesis output (the first bit)
    /// is retained.
    pub fn push(&mut self, retained: bool) -> Result<(), SpentnessHintsError> {
        if self.pushed >= self.output_count {
            return Err(SpentnessHintsError::Ordinal);
        }
        if self.pushed == 0 && retained {
            return Err(SpentnessHintsError::Format(
                "genesis output must not be retained",
            ));
        }
        if retained {
            let (byte, mask) = bit_position(self.pushed)?;
            self.bytes[byte] |= mask;
        }
        self.pushed += 1;
        Ok(())
    }

    /// Finishes the artifact.
    ///
    /// # Errors
    ///
    /// [`SpentnessHintsError::Format`] unless exactly `output_count` bits
    /// were pushed.
    pub fn finish(self) -> Result<Vec<u8>, SpentnessHintsError> {
        if self.pushed != self.output_count {
            return Err(SpentnessHintsError::Format(
                "pushed bit count does not match the output count",
            ));
        }
        Ok(self.bytes)
    }
}

/// How a node uses spentness hints (`[sync] v2_spentness_hints`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Never use hints. The default.
    #[default]
    Off,

    /// Use hints when the state starts empty and a verified artifact is
    /// acquired in time; otherwise sync unhinted.
    Auto,

    /// Refuse to sync an empty state below `H` without verified hints.
    Require,
}

/// Errors parsing, authenticating or reading an artifact.
#[derive(Error, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SpentnessHintsError {
    /// The bytes break the format.
    #[error("invalid spentness-hint artifact: {0}")]
    Format(&'static str),

    /// The artifact does not match its pinned [`MaxCheckpoint`] or network.
    #[error("spentness-hint artifact does not match its pinned checkpoint: {0}")]
    PinMismatch(&'static str),

    /// An output ordinal outside the artifact.
    #[error("spentness-hint output ordinal is out of range")]
    Ordinal,
}
