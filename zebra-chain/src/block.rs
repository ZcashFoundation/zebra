//! Blocks and block-related structures (heights, headers, etc.)

use std::{collections::HashMap, fmt, ops::Neg, sync::Arc};

use halo2::pasta::pallas;

use crate::{
    amount::{DeferredPoolBalanceChange, NegativeAllowed},
    block::merkle::AuthDataRoot,
    fmt::DisplayToDebug,
    orchard,
    parameters::{Network, NetworkUpgrade},
    sapling,
    serialization::TrustedPreallocate,
    sprout,
    transaction::Transaction,
    transparent,
    value_balance::{ValueBalance, ValueBalanceError},
};

mod commitment;
mod error;
mod hash;
mod header;
mod height;
mod serialize;

pub mod genesis;
pub mod merkle;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;
#[cfg(any(test, feature = "bench", feature = "proptest-impl"))]
pub mod tests;

pub use commitment::{
    ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Commitment, CommitmentError,
    CHAIN_HISTORY_ACTIVATION_RESERVED,
};
pub use hash::Hash;
pub use header::{BlockTimeError, CountedHeader, Header, ZCASH_BLOCK_VERSION};
pub use height::{Height, HeightDiff, TryIntoHeight};
pub use serialize::{SerializedBlock, MAX_BLOCK_BYTES};

#[cfg(any(test, feature = "proptest-impl"))]
pub use arbitrary::LedgerState;

/// A Zcash block, containing a header and a list of transactions.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl", feature = "elasticsearch"),
    derive(Serialize)
)]
pub struct Block {
    /// The block header, containing block metadata.
    pub header: Arc<Header>,
    /// The block transactions.
    pub transactions: Vec<Arc<Transaction>>,
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("Block");

        if let Some(height) = self.coinbase_height() {
            fmter.field("height", &height);
        }
        fmter.field("transactions", &self.transactions.len());
        fmter.field("hash", &DisplayToDebug(self.hash()));

        fmter.finish()
    }
}

impl Block {
    /// Return the block height reported in the coinbase transaction, if any.
    ///
    /// Note
    ///
    /// Verified blocks have a valid height.
    pub fn coinbase_height(&self) -> Option<Height> {
        self.transactions
            .first()
            .and_then(|tx| tx.inputs().first())
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
    pub fn commitment(&self, network: &Network) -> Result<Commitment, CommitmentError> {
        match self.coinbase_height() {
            None => Err(CommitmentError::MissingBlockHeight {
                block_hash: self.hash(),
            }),
            Some(height) => Commitment::from_bytes(*self.header.commitment_bytes, network, height),
        }
    }

    /// Check if the `network_upgrade` fields from each transaction in the block matches
    /// the network upgrade calculated from the `network` and block height.
    ///
    /// # Consensus
    ///
    /// > [NU5 onward] The nConsensusBranchId field MUST match the consensus branch ID used
    /// > for SIGHASH transaction hashes, as specified in [ZIP-244].
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
    ///
    /// [ZIP-244]: https://zips.z.cash/zip-0244
    #[allow(clippy::unwrap_in_result)]
    pub fn check_transaction_network_upgrade_consistency(
        &self,
        network: &Network,
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
            .flat_map(|transaction| transaction.sprout_nullifiers())
    }

    /// Access the [`sapling::Nullifier`]s from all transactions in this block.
    pub fn sapling_nullifiers(&self) -> impl Iterator<Item = &sapling::Nullifier> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sapling_nullifiers())
    }

    /// Access the [`orchard::Nullifier`]s from all transactions in this block.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = &orchard::Nullifier> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.orchard_nullifiers())
    }

    /// Access the [`sprout::NoteCommitment`]s from all transactions in this block.
    pub fn sprout_note_commitments(&self) -> impl Iterator<Item = &sprout::NoteCommitment> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sprout_note_commitments())
    }

    /// Access the [sapling note commitments](`sapling_crypto::note::ExtractedNoteCommitment`)
    /// from all transactions in this block.
    pub fn sapling_note_commitments(
        &self,
    ) -> impl Iterator<Item = &sapling_crypto::note::ExtractedNoteCommitment> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sapling_note_commitments())
    }

    /// Access the [orchard note commitments](pallas::Base) from all transactions in this block.
    pub fn orchard_note_commitments(&self) -> impl Iterator<Item = &pallas::Base> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.orchard_note_commitments())
    }

    /// Count how many Sapling transactions exist in a block,
    /// i.e. transactions "where either of vSpendsSapling or vOutputsSapling is non-empty"
    /// <https://zips.z.cash/zip-0221#tree-node-specification>.
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
    /// <https://zips.z.cash/zip-0221#tree-node-specification>.
    pub fn orchard_transactions_count(&self) -> u64 {
        self.transactions
            .iter()
            .filter(|tx| tx.has_orchard_shielded_data())
            .count()
            .try_into()
            .expect("number of transactions must fit u64")
    }

    /// Returns the overall chain value pool change in this block---the negative sum of the
    /// transaction value balances in this block.
    ///
    /// These are the changes in the transparent, Sprout, Sapling, Orchard, and
    /// Deferred chain value pools, as a result of this block.
    ///
    /// Positive values are added to the corresponding chain value pool and negative values are
    /// removed from the corresponding pool.
    ///
    /// <https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions>
    ///
    /// The given `utxos` must contain the [`transparent::Utxo`]s of every input in this block,
    /// including UTXOs created by earlier transactions in this block. It can also contain unrelated
    /// UTXOs, which are ignored.
    ///
    /// Note that the chain value pool has the opposite sign to the transaction value pool.
    pub fn chain_value_pool_change(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
        deferred_pool_balance_change: DeferredPoolBalanceChange,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        // `Result<T, E>` implements `IntoIterator`, so a `flat_map(|t| t.value_balance(utxos))`
        // would silently drop transactions whose value balance returns `Err`. Use `try_fold`
        // to propagate the first error instead.
        let tx_pool_sum = self
            .transactions
            .iter()
            .try_fold(ValueBalance::<NegativeAllowed>::zero(), |acc, tx| {
                acc + tx.value_balance(utxos)?
            })?;

        Ok(*tx_pool_sum
            .neg()
            .set_deferred_amount(deferred_pool_balance_change.value()))
    }

    /// Compute the root of the authorizing data Merkle tree,
    /// as defined in [ZIP-244].
    ///
    /// [ZIP-244]: https://zips.z.cash/zip-0244
    pub fn auth_data_root(&self) -> AuthDataRoot {
        self.transactions.iter().collect::<AuthDataRoot>()
    }
}

impl<'a> From<&'a Block> for Hash {
    fn from(block: &'a Block) -> Hash {
        block.header.as_ref().into()
    }
}

/// The maximum number of `block::Hash` entries Zebra will preallocate for in
/// a single peer-deserialized vector.
///
/// In the P2P protocol, `Vec<block::Hash>` appears as the `known_blocks` block
/// locator in `getblocks` and `getheaders` messages. The Bitcoin/Zcash
/// convention encodes locators with exponentially-spaced heights (1, 2, 3, …,
/// 10, 20, 40, …, genesis), giving `~log2(N) + 10` entries for chain length N.
/// For current Zcash chain heights (~3M blocks) a legitimate locator has ~32
/// entries.
///
/// We cap at 101 to match Bitcoin Core's `MAX_LOCATOR_SZ` constant
/// (`net_processing.cpp`), which zcashd inherits. This avoids any risk of
/// rejecting legitimate locators sent by compatible nodes that follow the
/// existing Bitcoin/Zcash protocol convention.
///
/// Without this cap, `Hash::max_allocation` was previously derived from
/// `MAX_PROTOCOL_MESSAGE_LEN / 32 = 65,535`, which allowed a remote peer to
/// force ~2 MiB heap preallocation per crafted `getblocks`/`getheaders` message
/// before any payload was read. This is the same class as
/// GHSA-xr93-pcq3-pxf8 (`addr_limit`), fixed for AddrV1/V2 in PR #10494.
pub const MAX_BLOCK_LOCATOR_LENGTH: u64 = 101;

impl TrustedPreallocate for Hash {
    fn max_allocation() -> u64 {
        MAX_BLOCK_LOCATOR_LENGTH
    }
}
