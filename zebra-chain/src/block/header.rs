//! The block header.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    fmt::HexDebug,
    parameters::Network,
    serialization::{TrustedPreallocate, MAX_PROTOCOL_MESSAGE_LEN},
    work::{difficulty::CompactDifficulty, equihash::Solution},
};

use super::{merkle, Commitment, CommitmentError, Hash, Height};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A block header, containing metadata about a block.
///
/// How are blocks chained together? They are chained together via the
/// backwards reference (previous header hash) present in the block
/// header. Each block points backwards to its parent, all the way
/// back to the genesis block (the first block in the blockchain).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// The block's version field. This is supposed to be `4`:
    ///
    /// > The current and only defined block version number for Zcash is 4.
    ///
    /// but this was not enforced by the consensus rules, and defective mining
    /// software created blocks with other versions, so instead it's effectively
    /// a free field. The only constraint is that it must be at least `4` when
    /// interpreted as an `i32`.
    pub version: u32,

    /// The hash of the previous block, used to create a chain of blocks back to
    /// the genesis block.
    ///
    /// This ensures no previous block can be changed without also changing this
    /// block's header.
    pub previous_block_hash: Hash,

    /// The root of the Bitcoin-inherited transaction Merkle tree, binding the
    /// block header to the transactions in the block.
    ///
    /// Note that because of a flaw in Bitcoin's design, the `merkle_root` does
    /// not always precisely bind the contents of the block (CVE-2012-2459). It
    /// is sometimes possible for an attacker to create multiple distinct sets of
    /// transactions with the same Merkle root, although only one set will be
    /// valid.
    pub merkle_root: merkle::Root,

    /// Zcash blocks contain different kinds of commitments to their contents,
    /// depending on the network and height.
    ///
    /// The interpretation of this field has been changed multiple times,
    /// without incrementing the block [`version`](Self::version). Therefore,
    /// this field cannot be parsed without the network and height. Use
    /// [`Block::commitment`](super::Block::commitment) to get the parsed
    /// [`Commitment`].
    pub commitment_bytes: HexDebug<[u8; 32]>,

    /// The block timestamp is a Unix epoch time (UTC) when the miner
    /// started hashing the header (according to the miner).
    pub time: DateTime<Utc>,

    /// An encoded version of the target threshold this block's header
    /// hash must be less than or equal to, in the same nBits format
    /// used by Bitcoin.
    ///
    /// For a block at block height `height`, bits MUST be equal to
    /// `ThresholdBits(height)`.
    ///
    /// [Bitcoin-nBits](https://bitcoin.org/en/developer-reference#target-nbits)
    pub difficulty_threshold: CompactDifficulty,

    /// An arbitrary field that miners can change to modify the header
    /// hash in order to produce a hash less than or equal to the
    /// target threshold.
    pub nonce: HexDebug<[u8; 32]>,

    /// The Equihash solution.
    pub solution: Solution,
}

/// TODO: Use this error as the source for zebra_consensus::error::BlockError::Time,
/// and make `BlockError::Time` add additional context.
/// See <https://github.com/ZcashFoundation/zebra/issues/1021> for more details.
#[allow(missing_docs)]
#[derive(Error, Debug)]
pub enum BlockTimeError {
    #[error("invalid time {0:?} in block header {1:?} {2:?}: block time is more than 2 hours in the future ({3:?}). Hint: check your machine's date, time, and time zone.")]
    InvalidBlockTime(
        DateTime<Utc>,
        crate::block::Height,
        crate::block::Hash,
        DateTime<Utc>,
    ),
}

impl Header {
    /// TODO: Inline this function into zebra_consensus::block::check::time_is_valid_at.
    /// See <https://github.com/ZcashFoundation/zebra/issues/1021> for more details.
    #[allow(clippy::unwrap_in_result)]
    pub fn time_is_valid_at(
        &self,
        now: DateTime<Utc>,
        height: &Height,
        hash: &Hash,
    ) -> Result<(), BlockTimeError> {
        let two_hours_in_the_future = now
            .checked_add_signed(Duration::hours(2))
            .expect("calculating 2 hours in the future does not overflow");
        if self.time <= two_hours_in_the_future {
            Ok(())
        } else {
            Err(BlockTimeError::InvalidBlockTime(
                self.time,
                *height,
                *hash,
                two_hours_in_the_future,
            ))?
        }
    }

    /// Get the parsed block [`Commitment`] for this header.
    /// Its interpretation depends on the given `network` and block `height`.
    pub fn commitment(
        &self,
        network: &Network,
        height: Height,
    ) -> Result<Commitment, CommitmentError> {
        Commitment::from_bytes_in_serialized_order(*self.commitment_bytes, network, height)
    }

    /// Compute the hash of this header.
    pub fn hash(&self) -> Hash {
        Hash::from(self)
    }
}

/// A header with a count of the number of transactions in its block.
/// This structure is used in the Bitcoin network protocol.
///
/// The transaction count field is always zero, so we don't store it in the struct.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct CountedHeader {
    /// The header for a block
    pub header: Arc<Header>,
}

/// The serialized size of a Zcash block header.
///
/// Includes the equihash input, 32-byte nonce, 3-byte equihash length field, and equihash solution.
const BLOCK_HEADER_LENGTH: usize =
    crate::work::equihash::Solution::INPUT_LENGTH + 32 + 3 + crate::work::equihash::SOLUTION_SIZE;

/// The minimum size for a serialized CountedHeader.
///
/// A CountedHeader has BLOCK_HEADER_LENGTH bytes + 1 or more bytes for the transaction count
pub(crate) const MIN_COUNTED_HEADER_LEN: usize = BLOCK_HEADER_LENGTH + 1;

/// The Zcash accepted block version.
///
/// The consensus rules do not force the block version to be this value but just equal or greater than it.
/// However, it is suggested that submitted block versions to be of this exact value.
pub const ZCASH_BLOCK_VERSION: u32 = 4;

impl TrustedPreallocate for CountedHeader {
    fn max_allocation() -> u64 {
        // Every vector type requires a length field of at least one byte for de/serialization.
        // Therefore, we can never receive more than (MAX_PROTOCOL_MESSAGE_LEN - 1) / MIN_COUNTED_HEADER_LEN counted headers in a single message
        ((MAX_PROTOCOL_MESSAGE_LEN - 1) / MIN_COUNTED_HEADER_LEN) as u64
    }
}
