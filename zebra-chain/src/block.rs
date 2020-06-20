//! Definitions of block datastructures.
#![allow(clippy::unit_arg)]

mod hash;
mod header;
mod serialize;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::transaction::Transaction;
use crate::types::BlockHeight;

pub use hash::BlockHeaderHash;
pub use header::BlockHeader;

/// A block in your blockchain.
///
/// A block is a data structure with two fields:
///
/// Block header: a data structure containing the block's metadata
/// Transactions: an array (vector in Rust) of transactions
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Block {
    /// The block header, containing block metadata.
    pub header: BlockHeader,
    /// The block transactions.
    pub transactions: Vec<Arc<Transaction>>,
}

/// The maximum size of a Zcash block, in bytes.
///
/// Post-Sapling, this is also the maximum size of a transaction
/// in the Zcash specification. (But since blocks also contain a
/// block header and transaction count, the maximum size of a
/// transaction in the chain is approximately 1.5 kB smaller.)
const MAX_BLOCK_BYTES: u64 = 2_000_000;

impl Block {
    /// Return the block height reported in the coinbase transaction, if any.
    pub fn coinbase_height(&self) -> Option<BlockHeight> {
        use crate::transaction::TransparentInput;
        self.transactions
            .get(0)
            .and_then(|tx| tx.inputs().next())
            .and_then(|input| match input {
                TransparentInput::Coinbase { ref height, .. } => Some(*height),
                _ => None,
            })
    }
}

impl<'a> From<&'a Block> for BlockHeaderHash {
    fn from(block: &'a Block) -> BlockHeaderHash {
        (&block.header).into()
    }
}
