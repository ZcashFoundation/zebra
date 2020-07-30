//! Block difficulty data structures and calculations
//!
//! The block difficulty "target threshold" is stored in the block header as a
//! 32-bit "compact bits" value. The `BlockHeaderHash` must be less than or equal
//! to the expanded target threshold, when represented as a 256-bit integer in
//! little-endian order.
//!
//! The target threshold is also used to calculate the "work" for each block.
//! The block work is used to find the chain with the greatest total work. Each
//! block's work value depends on the fixed threshold in the block header, not
//! the actual work represented by the block header hash.

#[cfg(test)]
use proptest_derive::Arbitrary;

/// A 32-bit "compact bits" value, which represents the difficulty threshold for
/// a block header.
///
/// Used for:
///   - checking the `difficulty_threshold` value in the block header,
///   - calculating the 256-bit `ExpandedDifficulty` threshold, for comparison
///     with the block header hash, and
///   - calculating the block work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct CompactDifficulty(pub u32);
