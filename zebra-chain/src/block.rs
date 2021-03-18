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
    serialization::{SafePreallocate, MAX_PROTOCOL_MESSAGE_LEN},
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

impl SafePreallocate for Hash {
    fn max_allocation() -> u64 {
        // A block Hash has takes 32 bytes so we can never receive more than (MAX_PROTOCOL_MESSAGE_LEN / 32) in a single message
        (MAX_PROTOCOL_MESSAGE_LEN as u64) / 32
    }
}

#[cfg(test)]
mod test_safe_preallocate {
    use super::{Hash, MAX_PROTOCOL_MESSAGE_LEN};
    use crate::serialization::{SafePreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;
    proptest! {

        #![proptest_config(ProptestConfig::with_cases(200))]


        #[test]
        fn block_hash_max_allocation(hash in Hash::arbitrary_with(())) {
            let max_allocation: usize = Hash::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Hash::max_allocation()+1) {
                smallest_disallowed_vec.push(hash.clone());
            }
            let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Hash::max_allocation());
            // Check that our smallest_disallowed_vec is too big to send as a protocol message
            prop_assert!(serialized.len() >= MAX_PROTOCOL_MESSAGE_LEN);
        }
    }
}
