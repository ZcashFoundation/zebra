//! Blocks and block-related structures (heights, headers, etc.)

mod hash;
mod header;
mod height;
mod root_hash;
mod serialize;

pub mod merkle;

#[cfg(test)]
mod tests;

pub use hash::BlockHeaderHash;
pub use header::BlockHeader;
pub use height::BlockHeight;
pub use root_hash::RootHash;

/// The error type for Block checks.
// XXX try to remove this -- block checks should be done in zebra-consensus
type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

use serde::{Deserialize, Serialize};

use crate::parameters::Network;
use crate::transaction::Transaction;

#[cfg(test)]
use proptest_derive::Arbitrary;

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
    pub transactions: Vec<std::sync::Arc<Transaction>>,
}

impl Block {
    /// Return the block height reported in the coinbase transaction, if any.
    pub fn coinbase_height(&self) -> Option<BlockHeight> {
        use crate::transaction::TransparentInput;
        self.transactions
            .get(0)
            .and_then(|tx| tx.inputs().get(0))
            .and_then(|input| match input {
                TransparentInput::Coinbase { ref height, .. } => Some(*height),
                _ => None,
            })
    }

    /// Check that there is exactly one coinbase transaction in `Block`, and that
    /// the coinbase transaction is the first transaction in the block.
    ///
    /// "The first (and only the first) transaction in a block is a coinbase
    /// transaction, which collects and spends any miner subsidy and transaction
    /// fees paid by transactions included in this block." [§3.10][3.10]
    ///
    /// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
    pub fn is_coinbase_first(&self) -> Result<(), Error> {
        let first = self
            .transactions
            .get(0)
            .ok_or_else(|| "block has no transactions")?;
        let mut rest = self.transactions.iter().skip(1);
        if !first.is_coinbase() {
            return Err("first transaction must be coinbase".into());
        }
        if rest.any(|tx| tx.contains_coinbase_input()) {
            return Err("coinbase input found in non-coinbase transaction".into());
        }
        Ok(())
    }

    /// Get the hash for the current block
    pub fn hash(&self) -> BlockHeaderHash {
        BlockHeaderHash::from(self)
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

impl<'a> From<&'a Block> for BlockHeaderHash {
    fn from(block: &'a Block) -> BlockHeaderHash {
        (&block.header).into()
    }
}
