//! Blocks and block-related structures (heights, headers, etc.)
#![allow(clippy::unit_arg)]

mod hash;
mod header;
mod height;
mod root_hash;
mod serialize;

pub mod merkle;

#[cfg(test)]
mod tests;

pub use hash::Hash;
pub use header::Header;
pub use height::Height;
pub use root_hash::RootHash;

/// The error type for Block checks.
// XXX try to remove this -- block checks should be done in zebra-consensus
type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

use serde::{Deserialize, Serialize};

use crate::{parameters::Network, transaction::Transaction, transparent};

#[cfg(test)]
use proptest_derive::Arbitrary;

/// A Zcash block, containing a header and a list of transactions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Block {
    /// The block header, containing block metadata.
    pub header: Header,
    /// The block transactions.
    pub transactions: Vec<std::sync::Arc<Transaction>>,
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
        match self.coinbase_height() {
            Some(height) => Some(RootHash::from_bytes(
                self.header.root_bytes,
                network,
                height,
            )),
            None => None,
        }
    }
}

impl<'a> From<&'a Block> for Hash {
    fn from(block: &'a Block) -> Hash {
        (&block.header).into()
    }
}
