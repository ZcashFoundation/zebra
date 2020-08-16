use chrono::{DateTime, Duration, Utc};

use crate::serialization::ZcashSerialize;
use crate::work::{difficulty::CompactDifficulty, equihash::Solution};

use super::{merkle, Error, Hash};

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
    /// block’s header.
    pub previous_block_hash: Hash,

    /// The root of the transaction Merkle tree.
    ///
    /// The Merkle root is derived from the SHA256d hashes of all transactions
    /// included in this block as assembled in a binary tree, ensuring that none
    /// of those transactions can be modied without modifying the header.
    pub merkle_root: merkle::Root,

    /// Some kind of root hash.
    ///
    /// Unfortunately, the interpretation of this field was changed without
    /// incrementing the version, so it cannot be parsed without the block height
    /// and network. Use [`Block::root_hash`](super::Block::root_hash) to get the
    /// parsed [`RootHash`](super::RootHash).
    pub root_bytes: [u8; 32],

    /// The block timestamp is a Unix epoch time (UTC) when the miner
    /// started hashing the header (according to the miner).
    pub time: DateTime<Utc>,

    /// An encoded version of the target threshold this block’s header
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

impl Header {
    /// Returns true if the header is valid based on its `EquihashSolution`
    pub fn is_equihash_solution_valid(&self) -> Result<(), EquihashError> {
        let n = 200;
        let k = 9;
        let nonce = &self.nonce;
        let solution = &self.solution.0;
        let mut input = Vec::new();

        self.zcash_serialize(&mut input)
            .expect("serialization into a vec can't fail");

        let input = &input[0..Solution::INPUT_LENGTH];

        equihash::is_valid_solution(n, k, input, nonce, solution)?;

        Ok(())
    }

    /// Check if `self.time` is less than or equal to
    /// 2 hours in the future, according to the node's local clock (`now`).
    ///
    /// This is a non-deterministic rule, as clocks vary over time, and
    /// between different nodes.
    ///
    /// "In addition, a full validator MUST NOT accept blocks with nTime
    /// more than two hours in the future according to its clock. This
    /// is not strictly a consensus rule because it is nondeterministic,
    /// and clock time varies between nodes. Also note that a block that
    /// is rejected by this rule at a given point in time may later be
    /// accepted." [§7.5][7.5]
    ///
    /// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
    pub fn is_time_valid_at(&self, now: DateTime<Utc>) -> Result<(), Error> {
        let two_hours_in_the_future = now
            .checked_add_signed(Duration::hours(2))
            .ok_or("overflow when calculating 2 hours in the future")?;
        if self.time <= two_hours_in_the_future {
            Ok(())
        } else {
            Err("block header time is more than 2 hours in the future".into())
        }
    }
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
#[error("invalid equihash solution for BlockHeader")]
pub struct EquihashError(#[from] equihash::Error);
