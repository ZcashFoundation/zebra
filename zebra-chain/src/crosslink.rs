//! Core Zcash Crosslink data structures.
//!
//! This module contains in-memory state validation types for the Zcash Trailing Finality Layer
//! (TFL). It excludes I/O, tokio, services, etc.
#![allow(missing_docs)]

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use thiserror::Error;
use tracing::error;

use crate::block::Header as BcBlockHeader;
use crate::block::FatPointerToBftBlock;
use crate::serialization::{
    ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// The BLAKE3 key used for hashing BFT block values.
///
/// This is derived from the context string "BFT Value ID" using BLAKE3's key derivation.
/// It was originally defined in tenderlink as `HashKeys::default().value_id`.
pub fn bft_value_id_hash_key() -> [u8; 32] {
    blake3::Hasher::new_derive_key("BFT Value ID")
        .finalize()
        .into()
}

/// When true, use an unkeyed BLAKE3 hasher for BFT block hashing.
///
/// The existing test data was generated with the unkeyed hasher.
/// TODO: Regenerate test data with the keyed hasher and remove this flag.
pub static BFT_HASH_USE_UNKEYED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The BFT block content for Crosslink.
///
/// # Constructing [BftBlock]s
///
/// A [BftBlock] may be constructed from a node's local view in order to create a new BFT
/// proposal, or they may be constructed from unknown sources across a network protocol.
///
/// To construct a [BftBlock] for a new BFT proposal, build a [Vec] of [BcBlockHeader] values,
/// starting from the latest known PoW tip and traversing back in time (following
/// [previous_block_hash](BcBlockHeader::previous_block_hash)) until exactly
/// [bc_confirmation_depth_sigma](ZcashCrosslinkParameters::bc_confirmation_depth_sigma) headers
/// are collected, then pass this to [BftBlock::try_from].
///
/// To construct from an untrusted source, call the same [BftBlock::try_from].
///
/// ## Validation and Limitations
///
/// The [BftBlock::try_from] method is the only way to construct [BftBlock] values and performs
/// the following validation internally:
///
/// 1. The number of headers matches the expected protocol confirmation depth,
///    [bc_confirmation_depth_sigma](ZcashCrosslinkParameters::bc_confirmation_depth_sigma).
/// 2. The [version](BcBlockHeader::version) field is a known expected value.
/// 3. The headers are in the correct order given the
///    [previous_block_hash](BcBlockHeader::previous_block_hash) fields.
/// 4. The PoW solutions validate.
///
/// These validations use *immediate data* and are *stateless*, and in particular the following
/// stateful validations are **NOT** performed:
///
/// 1. The [difficulty_threshold](BcBlockHeader::difficulty_threshold) is within correct bounds
///    for the Difficulty Adjustment Algorithm.
/// 2. The [time](BcBlockHeader::time) field is within correct bounds.
/// 3. The [merkle_root](BcBlockHeader::merkle_root) field is sensible.
///
/// No other validations are performed.
///
/// **TODO:** Ensure deserialization delegates to [BftBlock::try_from].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BftBlock {
    /// The Version Number
    pub version: u32,
    /// The Height of this BFT Payload
    pub height: u32,
    /// Hash of the previous BFT Block.
    pub previous_block_fat_ptr: FatPointerToBftBlock,
    /// The height of the PoW block that is the finalization candidate.
    pub finalization_candidate_height: u32,
    /// The PoW Headers
    pub headers: Vec<BcBlockHeader>,
}

impl ZcashSerialize for BftBlock {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(self.version)?;
        writer.write_u32::<LittleEndian>(self.height)?;
        self.previous_block_fat_ptr.zcash_serialize(&mut writer)?;
        writer.write_u32::<LittleEndian>(self.finalization_candidate_height)?;
        writer.write_u32::<LittleEndian>(self.headers.len().try_into().unwrap())?;
        for header in &self.headers {
            header.zcash_serialize(&mut writer)?;
        }
        Ok(())
    }
}

impl ZcashDeserialize for BftBlock {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let version = reader.read_u32::<LittleEndian>()?;
        let height = reader.read_u32::<LittleEndian>()?;
        let previous_block_fat_ptr = FatPointerToBftBlock::zcash_deserialize(&mut reader)?;
        let finalization_candidate_height = reader.read_u32::<LittleEndian>()?;
        let header_count = reader.read_u32::<LittleEndian>()?;
        if header_count > 2048 {
            return Err(SerializationError::Parse(
                "header_count was greater than 2048.",
            ));
        }
        let mut array = Vec::new();
        for _i in 0..header_count {
            array.push(crate::block::Header::zcash_deserialize(&mut reader)?);
        }

        Ok(BftBlock {
            version,
            height,
            previous_block_fat_ptr,
            finalization_candidate_height,
            headers: array,
        })
    }
}

impl BftBlock {
    /// Refer to the [BcBlockHeader] that is the finalization candidate for this block.
    ///
    /// **UNVERIFIED:** The finalization_candidate of a final [BftBlock] is finalized.
    pub fn finalization_candidate(&self) -> &BcBlockHeader {
        self.headers.last().expect("Vec should never be empty")
    }

    /// Attempt to construct a [BftBlock] from headers while performing immediate validations.
    pub fn try_from(
        params: &ZcashCrosslinkParameters,
        height: u32,
        previous_block_fat_ptr: FatPointerToBftBlock,
        finalization_candidate_height: u32,
        headers: Vec<BcBlockHeader>,
    ) -> Result<Self, InvalidBftBlock> {
        let expected = params.bc_confirmation_depth_sigma;
        let actual = headers.len() as u64;
        if actual != expected {
            return Err(InvalidBftBlock::IncorrectConfirmationDepth { expected, actual });
        }

        error!("not yet implemented: all the documented validations");

        Ok(BftBlock {
            version: 1,
            height,
            previous_block_fat_ptr,
            finalization_candidate_height,
            headers,
        })
    }

    /// Hash for the block.
    pub fn blake3_hash(&self) -> Blake3Hash {
        self.into()
    }

    /// Just the hash of the previous block, which identifies it but does not provide any
    /// guarantees. Consider using the `previous_block_fat_ptr` instead.
    pub fn previous_block_hash(&self) -> Blake3Hash {
        Blake3Hash(self.previous_block_fat_ptr.points_at_block_hash())
    }
}

impl<'a> From<&'a BftBlock> for Blake3Hash {
    fn from(block: &'a BftBlock) -> Self {
        let mut hash_writer = if BFT_HASH_USE_UNKEYED.load(std::sync::atomic::Ordering::Relaxed) {
            // Test data was generated with the unkeyed hasher.
            // TODO: Regenerate test data with the keyed hasher and remove this branch.
            blake3::Hasher::new()
        } else {
            blake3::Hasher::new_keyed(&bft_value_id_hash_key())
        };
        block
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finalize().into())
    }
}

/// Validation error for [BftBlock]
#[derive(Debug, Error)]
pub enum InvalidBftBlock {
    /// An incorrect number of headers was present
    #[error(
        "invalid confirmation depth: Crosslink requires {expected} while {actual} were present"
    )]
    IncorrectConfirmationDepth {
        /// The expected number of headers
        expected: u64,
        /// The number of headers present
        actual: u64,
    },
}

/// Zcash Crosslink protocol parameters.
///
/// Ref: [Zcash Trailing Finality Layer §3.3.3 Parameters](https://electric-coin-company.github.io/tfl-book/design/crosslink/construction.html#parameters)
#[derive(Clone, Debug)]
pub struct ZcashCrosslinkParameters {
    /// The best-chain confirmation depth, `σ`
    pub bc_confirmation_depth_sigma: u64,
    /// The depth of unfinalized PoW blocks past which "Stalled Mode" activates, `L`
    pub finalization_gap_bound: u64,
}

/// Crosslink parameters chosen for prototyping / testing.
///
/// No verification has been done on the security or performance of these parameters.
pub const PROTOTYPE_PARAMETERS: ZcashCrosslinkParameters = ZcashCrosslinkParameters {
    bc_confirmation_depth_sigma: 3,
    finalization_gap_bound: 7,
};

/// A BLAKE3 hash.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Serialize, Deserialize)]
pub struct Blake3Hash(pub [u8; 32]);

impl std::fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for &b in self.0.iter() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for &b in self.0.iter() {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl ZcashSerialize for Blake3Hash {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&self.0)?;
        Ok(())
    }
}

impl ZcashDeserialize for Blake3Hash {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Blake3Hash(reader.read_32_bytes()?))
    }
}

/// A BFT block and the fat pointer that shows it has been signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BftBlockAndFatPointerToIt {
    /// A BFT block
    pub block: BftBlock,
    /// The fat pointer to block, showing it has been signed
    pub fat_ptr: FatPointerToBftBlock,
}

impl ZcashDeserialize for BftBlockAndFatPointerToIt {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(BftBlockAndFatPointerToIt {
            block: BftBlock::zcash_deserialize(&mut reader)?,
            fat_ptr: FatPointerToBftBlock::zcash_deserialize(&mut reader)?,
        })
    }
}

impl ZcashSerialize for BftBlockAndFatPointerToIt {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.block.zcash_serialize(&mut writer)?;
        self.fat_ptr.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

/// A vote for a value in a round of BFT consensus.
///
/// ## Data Layout (76 bytes total)
///
/// - 32 byte ed25519 public key of the finalizer
/// - 32 byte blake3 hash of value, or all zeroes to indicate Nil vote
/// - 8 byte height
/// - 4 byte round where MSB is used to indicate is_commit for the vote type.
///   1 bit is_commit, 31 bits round index
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BftVote {
    pub validator_address: BftValidatorAddress,
    pub value: Blake3Hash,
    pub height: u64,
    /// true is commit (precommit), false is prevote
    pub typ: bool,
    pub round: i32,
}

impl BftVote {
    /// Serialize this vote to a fixed-size 76-byte array.
    pub fn to_bytes(&self) -> [u8; 76] {
        let mut buf = [0_u8; 76];
        buf[0..32].copy_from_slice(self.validator_address.0.as_ref());
        buf[32..64].copy_from_slice(&self.value.0);
        buf[64..72].copy_from_slice(&self.height.to_le_bytes());

        let mut merged_round_val: u32 = (self.round & 0x7fff_ffff) as u32;
        if self.typ {
            merged_round_val |= 0x8000_0000;
        }
        buf[72..76].copy_from_slice(&merged_round_val.to_le_bytes());
        buf
    }

    /// Deserialize a vote from a fixed-size 76-byte array.
    pub fn from_bytes(bytes: &[u8; 76]) -> BftVote {
        let validator_address = BftValidatorAddress(
            ed25519_zebra::VerificationKeyBytes::from(<[u8; 32]>::try_from(&bytes[0..32]).unwrap()),
        );
        let value = Blake3Hash(bytes[32..64].try_into().unwrap());
        let height = u64::from_le_bytes(bytes[64..72].try_into().unwrap());

        let merged_round_val = u32::from_le_bytes(bytes[72..76].try_into().unwrap());

        let typ = merged_round_val & 0x8000_0000 != 0;
        let round = (merged_round_val & 0x7fff_ffff) as i32;

        BftVote {
            validator_address,
            value,
            height,
            typ,
            round,
        }
    }
}

/// A wrapper around an ed25519 public key used as a BFT validator address.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BftValidatorAddress(pub ed25519_zebra::VerificationKeyBytes);

impl std::fmt::Display for BftValidatorAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0.as_ref() {
            write!(f, "{:02X}", byte)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for BftValidatorAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

/// A BFT validator with public key and voting power (stake).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BftValidator {
    pub address: BftValidatorAddress,
    pub public_key: ed25519_zebra::VerificationKeyBytes,
    pub voting_power: u64,
}

impl BftValidator {
    /// Create a new validator with the given public key and voting power.
    pub fn new(public_key: ed25519_zebra::VerificationKeyBytes, voting_power: u64) -> Self {
        Self {
            address: BftValidatorAddress(public_key),
            public_key,
            voting_power,
        }
    }
}

impl PartialOrd for BftValidator {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BftValidator {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.address.cmp(&other.address)
    }
}

/// The result of validating a BFT block.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BftValidationStatus {
    /// Validation is indeterminate (not enough information yet)
    Indeterminate,
    /// Validation passed (2f+1 yes)
    Pass,
    /// Validation failed (f+1 no)
    Fail,
}

// Methods on FatPointerToBftBlock that depend on crosslink types.
impl FatPointerToBftBlock {
    /// Get a [BftVote] template from this fat pointer (with zeroed validator address).
    pub fn get_vote_template(&self) -> BftVote {
        let mut vote_bytes = [0_u8; 76];
        vote_bytes[32..76].copy_from_slice(&self.vote_for_block_without_finalizer_public_key);
        BftVote::from_bytes(&vote_bytes)
    }

    /// Inflate this fat pointer into a list of individual votes with their signatures.
    pub fn inflate(&self) -> Vec<(BftVote, ed25519_zebra::ed25519::SignatureBytes)> {
        let vote_template = self.get_vote_template();
        self.signatures
            .iter()
            .map(|s| {
                let mut vote = vote_template.clone();
                vote.validator_address = BftValidatorAddress(
                    ed25519_zebra::VerificationKeyBytes::from(s.public_key),
                );
                (vote, s.vote_signature)
            })
            .collect()
    }

    /// Validate that all ed25519 signatures in this fat pointer are correct.
    pub fn validate_signatures(&self) -> bool {
        let mut batch = ed25519_zebra::batch::Verifier::new();
        for (vote, signature) in self.inflate() {
            let vk_bytes = ed25519_zebra::VerificationKeyBytes::from(vote.validator_address.0);
            let sig = ed25519_zebra::Signature::from_bytes(&signature);
            let msg = vote.to_bytes();

            batch.queue((vk_bytes, sig, &msg));
        }
        batch.verify(rand_core::OsRng).is_ok()
    }

    /// Get the [Blake3Hash] of the BFT block this fat pointer points at.
    pub fn points_at_blake3_hash(&self) -> Blake3Hash {
        Blake3Hash(self.points_at_block_hash())
    }
}

/// Placeholder activation height for Crosslink functionality.
pub const TFL_ACTIVATION_HEIGHT: crate::block::Height = crate::block::Height(2000);
