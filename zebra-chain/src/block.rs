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
#[cfg(test)]
mod tests;

use std::fmt;

pub use commitment::Commitment;
pub use hash::Hash;
pub use header::BlockTimeError;
pub use header::{CountedHeader, Header};
pub use height::Height;

use serde::{Deserialize, Serialize};

use crate::{fmt::DisplayToDebug, parameters::Network, transaction::Transaction, transparent};

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
    /// Returns None if this block does not have a block height.
    pub fn commitment(&self, network: Network) -> Option<Commitment> {
        self.coinbase_height()
            .map(|height| Commitment::from_bytes(self.header.commitment_bytes, network, height))
    }
}

impl<'a> From<&'a Block> for Hash {
    fn from(block: &'a Block) -> Hash {
        (&block.header).into()
    }
}
