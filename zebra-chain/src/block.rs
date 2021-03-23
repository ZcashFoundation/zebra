//! Blocks and block-related structures (heights, headers, etc.)
#![allow(clippy::unit_arg)]

mod hash;
mod header;
mod height;
mod root_hash;
mod serialize;

pub mod merkle;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

use std::fmt;

pub use hash::Hash;
pub use header::BlockTimeError;
pub use header::{CountedHeader, Header};
pub use height::Height;
pub use root_hash::RootHash;
pub use serialize::MAX_BLOCK_BYTES;

use serde::{Deserialize, Serialize};

use crate::{
    fmt::DisplayToDebug,
    parameters::Network,
    serialization::{TrustedPreallocate, MAX_PROTOCOL_MESSAGE_LEN},
    transaction::Transaction,
    transparent,
};

/// A Zcash block, containing a header and a list of transactions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// The block header, containing block metadata.
    pub header: Header,
    /// The block transactions.
    pub transactions: Vec<std::sync::Arc<Transaction>>,
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("Block");
        if let Some(height) = self.coinbase_height() {
            fmter.field("height", &height);
        }

        fmter.field("hash", &DisplayToDebug(self.hash())).finish()
    }
}

impl Block {
    /// Return the block height reported in the coinbase transaction, if any.
    pub fn coinbase_height(&self) -> Option<Height> {
        self.transactions
            .get(0)
            .and_then(|tx| tx.inputs().get(0))
            .and_then(|input| match input {
                transparent::Input::Coinbase { ref height, .. } => Some(*height),
                _ => None,
            })
    }

    /// Compute the hash of this block.
    pub fn hash(&self) -> Hash {
        Hash::from(self)
    }

    /// Get the parsed root hash for this block.
    ///
    /// The interpretation of the root hash depends on the
    /// configured `network`, and this block's height.
    ///
    /// Returns None if this block does not have a block height.
    pub fn root_hash(&self, network: Network) -> Option<RootHash> {
        self.coinbase_height()
            .map(|height| RootHash::from_bytes(self.header.root_bytes, network, height))
    }
}

impl<'a> From<&'a Block> for Hash {
    fn from(block: &'a Block) -> Hash {
        (&block.header).into()
    }
}
/// A serialized Block hash takes 32 bytes
const BLOCK_HASH_SIZE: u64 = 32;
/// The maximum number of hashes in a valid Zcash protocol message.
impl TrustedPreallocate for Hash {
    fn max_allocation() -> u64 {
        // Every vector type requires a length field of at least one byte for de/serialization.
        // Since a block::Hash takes 32 bytes, we can never receive more than (MAX_PROTOCOL_MESSAGE_LEN - 1) / 32 hashes in a single message
        ((MAX_PROTOCOL_MESSAGE_LEN - 1) as u64) / BLOCK_HASH_SIZE
    }
}

#[cfg(test)]
mod test_trusted_preallocate {
    use super::{Hash, BLOCK_HASH_SIZE, MAX_PROTOCOL_MESSAGE_LEN};
    use crate::serialization::{TrustedPreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]
        /// Verify that the serialized size of a block hash used to calculate the allocation limit is correct
        #[test]
        fn block_hash_size_is_correct(hash in Hash::arbitrary()) {
            let serialized = hash.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 == BLOCK_HASH_SIZE);
        }
    }
    proptest! {

        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Verify that...
        /// 1. The smallest disallowed vector of `Hash`s is too large to send via the Zcash Wire Protocol
        /// 2. The largest allowed vector is small enough to fit in a legal Zcash Wire Protocol message
        #[test]
        fn block_hash_max_allocation(hash in Hash::arbitrary_with(())) {
            let max_allocation: usize = Hash::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Hash::max_allocation()+1) {
                smallest_disallowed_vec.push(hash);
            }

            let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Hash::max_allocation());
            // Check that our smallest_disallowed_vec is too big to send as a protocol message
            prop_assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

            // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
            smallest_disallowed_vec.pop();
            let largest_allowed_vec = smallest_disallowed_vec;
            let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our largest_allowed_vec contains the maximum number of hashes
            prop_assert!((largest_allowed_vec.len() as u64) == Hash::max_allocation());
            // Check that our largest_allowed_vec is small enough to send as a protocol message
            prop_assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);

        }
    }
}
