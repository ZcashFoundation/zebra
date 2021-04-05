use std::usize;

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    serialization::{TrustedPreallocate, MAX_PROTOCOL_MESSAGE_LEN},
    work::{difficulty::CompactDifficulty, equihash::Solution},
};

use super::{merkle, Hash, Height};

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

    /// Some kind of root hash.
    ///
    /// Unfortunately, the interpretation of this field was changed without
    /// incrementing the version, so it cannot be parsed without the block height
    /// and network. Use [`Block::commitment`](super::Block::commitment) to get the
    /// parsed [`Commitment`](super::Commitment).
    pub commitment_bytes: [u8; 32],

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
    pub nonce: [u8; 32],

    /// The Equihash solution.
    pub solution: Solution,
}

/// TODO: Use this error as the source for zebra_consensus::error::BlockError::Time,
/// and make `BlockError::Time` add additional context.
/// See https://github.com/ZcashFoundation/zebra/issues/1021 for more details.
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
    /// See https://github.com/ZcashFoundation/zebra/issues/1021 for more details.
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
}

/// A header with a count of the number of transactions in its block.
///
/// This structure is used in the Bitcoin network protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountedHeader {
    pub header: Header,
    pub transaction_count: usize,
}

/// The serialized size of a Zcash block header.
///
/// Includes the equihash input, 32-byte nonce, 3-byte equihash length field, and equihash solution.
const BLOCK_HEADER_LENGTH: usize =
    crate::work::equihash::Solution::INPUT_LENGTH + 32 + 3 + crate::work::equihash::SOLUTION_SIZE;

/// The minimum size for a serialized CountedHeader.
///
/// A CountedHeader has BLOCK_HEADER_LENGTH bytes + 1 or more bytes for the transaction count
const MIN_COUNTED_HEADER_LEN: usize = BLOCK_HEADER_LENGTH + 1;
impl TrustedPreallocate for CountedHeader {
    fn max_allocation() -> u64 {
        // Every vector type requires a length field of at least one byte for de/serialization.
        // Therefore, we can never receive more than (MAX_PROTOCOL_MESSAGE_LEN - 1) / MIN_COUNTED_HEADER_LEN counted headers in a single message
        ((MAX_PROTOCOL_MESSAGE_LEN - 1) / MIN_COUNTED_HEADER_LEN) as u64
    }
}

#[cfg(test)]
mod test_trusted_preallocate {
    use super::{CountedHeader, Header, MAX_PROTOCOL_MESSAGE_LEN, MIN_COUNTED_HEADER_LEN};
    use crate::serialization::{TrustedPreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;
    proptest! {

        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Confirm that each counted header takes at least COUNTED_HEADER_LEN bytes when serialized.
        /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn counted_header_min_length(header in Header::arbitrary_with(()), transaction_count in (0..std::u32::MAX)) {
            let header = CountedHeader {
                header,
                transaction_count: transaction_count.try_into().expect("Must run test on platform with at least 32 bit address space"),
            };
            let serialized_header = header.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized_header.len() >= MIN_COUNTED_HEADER_LEN)
        }
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        /// Verify that...
        /// 1. The smallest disallowed vector of `CountedHeaders`s is too large to send via the Zcash Wire Protocol
        /// 2. The largest allowed vector is small enough to fit in a legal Zcash Wire Protocol message
        #[test]
        fn counted_header_max_allocation(header in Header::arbitrary_with(())) {
            let header = CountedHeader {
                header,
                transaction_count: 0,
            };
            let max_allocation: usize = CountedHeader::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(CountedHeader::max_allocation()+1) {
                smallest_disallowed_vec.push(header.clone());
            }
            let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == CountedHeader::max_allocation());
            // Check that our smallest_disallowed_vec is too big to send as a protocol message
            prop_assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);


            // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
            smallest_disallowed_vec.pop();
            let largest_allowed_vec = smallest_disallowed_vec;
            let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our largest_allowed_vec contains the maximum number of CountedHeaders
            prop_assert!((largest_allowed_vec.len() as u64) == CountedHeader::max_allocation());
            // Check that our largest_allowed_vec is small enough to send as a protocol message
            prop_assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
        }
    }
}
