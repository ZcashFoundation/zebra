//! Blocks and block-related structures (heights, headers, etc.)

mod commitment;
mod error;
mod hash;
mod header;
mod height;
mod serialize;

pub mod merkle;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(any(test, feature = "bench"))]
pub mod tests;

use std::{convert::TryInto, fmt};

pub use commitment::{ChainHistoryMmrRootHash, Commitment, CommitmentError};
pub use hash::Hash;
pub use header::{BlockTimeError, CountedHeader, Header};
pub use height::Height;
pub use serialize::MAX_BLOCK_BYTES;

#[cfg(any(test, feature = "proptest-impl"))]
pub use arbitrary::LedgerState;

use serde::{Deserialize, Serialize};

use crate::{
    fmt::DisplayToDebug,
    orchard,
    parameters::{Network, NetworkUpgrade},
    sapling,
    serialization::{TrustedPreallocate, MAX_PROTOCOL_MESSAGE_LEN},
    sprout,
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

    /// Check if the `network_upgrade` fields from each transaction in the block matches
    /// the network upgrade calculated from the `network` and block height.
    ///
    /// # Consensus rule:
    ///
    ///  The nConsensusBranchId field MUST match the consensus branch ID used for
    ///  SIGHASH transaction hashes, as specifed in [ZIP-244] ([7.1]).
    ///
    /// [ZIP-244]: https://zips.z.cash/zip-0244
    /// [7.1]: https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
    pub fn check_transaction_network_upgrade_consistency(
        &self,
        network: Network,
    ) -> Result<(), error::BlockError> {
        let block_nu =
            NetworkUpgrade::current(network, self.coinbase_height().expect("a valid height"));

        if self
            .transactions
            .iter()
            .filter_map(|trans| trans.as_ref().network_upgrade())
            .any(|trans_nu| trans_nu != block_nu)
        {
            return Err(error::BlockError::WrongTransactionConsensusBranchId);
        }

        Ok(())
    }

    /// Access the [`sprout::Nullifier`]s from all transactions in this block.
    pub fn sprout_nullifiers(&self) -> impl Iterator<Item = &sprout::Nullifier> {
        self.transactions
            .iter()
            .map(|transaction| transaction.sprout_nullifiers())
            .flatten()
    }

    /// Access the [`sapling::Nullifier`]s from all transactions in this block.
    pub fn sapling_nullifiers(&self) -> impl Iterator<Item = &sapling::Nullifier> {
        self.transactions
            .iter()
            .map(|transaction| transaction.sapling_nullifiers())
            .flatten()
    }

    /// Access the [`orchard::Nullifier`]s from all transactions in this block.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = &orchard::Nullifier> {
        self.transactions
            .iter()
            .map(|transaction| transaction.orchard_nullifiers())
            .flatten()
    }

    /// Count how many Sapling transactions exist in a block,
    /// i.e. transactions "where either of vSpendsSapling or vOutputsSapling is non-empty"
    /// (https://zips.z.cash/zip-0221#tree-node-specification).
    pub fn sapling_transactions_count(&self) -> u64 {
        self.transactions
            .iter()
            .filter(|tx| tx.has_sapling_shielded_data())
            .count()
            .try_into()
            .expect("number of transactions must fit u64")
    }

    /// Count how many Orchard transactions exist in a block,
    /// i.e. transactions "where vActionsOrchard is non-empty."
    /// (https://zips.z.cash/zip-0221#tree-node-specification).
    pub fn orchard_transactions_count(&self) -> u64 {
        self.transactions
            .iter()
            .filter(|tx| tx.has_orchard_shielded_data())
            .count()
            .try_into()
            .expect("number of transactions must fit u64")
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
