//! Blocks and block-related structures (heights, headers, etc.)
#![allow(clippy::unit_arg)]

mod commitment;
mod hash;
mod header;
mod height;
mod serialize;

pub mod merkle;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(any(test, feature = "bench"))]
pub mod tests;

use std::fmt;

pub use commitment::{Commitment, CommitmentError};
pub use hash::Hash;
pub use header::{BlockTimeError, CountedHeader, Header};
pub use height::Height;
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

    /// Get the parsed block [`Commitment`] for this block.
    ///
    /// The interpretation of the commitment depends on the
    /// configured `network`, and this block's height.
    ///
    /// Returns an error if this block does not have a block height,
    /// or if the commitment value is structurally invalid.
    pub fn commitment(&self, network: Network) -> Result<Commitment, CommitmentError> {
        match self.coinbase_height() {
            None => Err(CommitmentError::MissingBlockHeight {
                block_hash: self.hash(),
            }),
            Some(height) => Commitment::from_bytes(self.header.commitment_bytes, network, height),
        }
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
