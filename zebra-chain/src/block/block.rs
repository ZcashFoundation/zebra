use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::parameters::Network;
use crate::transaction::Transaction;

use super::{BlockHeader, BlockHeaderHash, BlockHeight, Error, RootHash};

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
    pub transactions: Vec<Arc<Transaction>>,
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
            Err("first transaction must be coinbase")?;
        }
        if rest.any(|tx| tx.contains_coinbase_input()) {
            Err("coinbase input found in non-coinbase transaction")?;
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
