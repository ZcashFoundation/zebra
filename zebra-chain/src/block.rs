//! Blocks and block-related structures (heights, headers, etc.)

use std::{collections::HashMap, fmt, ops::Neg, sync::Arc};

use halo2::pasta::{group::ff::PrimeField, pallas};

use crate::{
    amount::{Amount, DeferredPoolBalanceChange, NegativeAllowed, NonNegative},
    block::merkle::AuthDataRoot,
    fmt::DisplayToDebug,
    ironwood, orchard,
    parameters::{subsidy, Network, NetworkUpgrade},
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
            .and_then(|tx| {
                let inputs = tx.inputs();
                inputs.into_iter().next()
            })
            .and_then(|input| match input {
                transparent::Input::Coinbase { height, .. } => Some(height),
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

    /// Access the sprout nullifiers from all transactions in this block.
    pub fn sprout_nullifiers(&self) -> impl Iterator<Item = sprout::Nullifier> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sprout_nullifiers().collect::<Vec<_>>())
    }

    /// Access the sapling nullifiers from all transactions in this block.
    pub fn sapling_nullifiers(&self) -> impl Iterator<Item = sapling::Nullifier> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sapling_nullifiers().collect::<Vec<_>>())
    }

    /// Access the orchard nullifiers from all transactions in this block.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = orchard::Nullifier> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.orchard_nullifiers().collect::<Vec<_>>())
    }

    /// Access the ironwood nullifiers from all transactions in this block.
    pub fn ironwood_nullifiers(&self) -> impl Iterator<Item = ironwood::Nullifier> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.ironwood_nullifiers().collect::<Vec<_>>())
    }

    /// Access the sprout note commitments from all transactions in this block.
    pub fn sprout_note_commitments(
        &self,
    ) -> impl Iterator<Item = sprout::commitment::NoteCommitment> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sprout_note_commitments().collect::<Vec<_>>())
    }

    /// Access the sapling note commitments from all transactions in this block.
    pub fn sapling_note_commitments(
        &self,
    ) -> impl Iterator<Item = sapling_crypto::note::ExtractedNoteCommitment> + '_ {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.sapling_note_commitments().collect::<Vec<_>>())
    }

    /// Access the orchard note commitments from all transactions in this block,
    /// as `pallas::Base` values for the note commitment tree.
    pub fn orchard_note_commitments(&self) -> impl Iterator<Item = pallas::Base> + '_ {
        self.transactions.iter().flat_map(|transaction| {
            transaction
                .orchard_note_commitments()
                .map(|cmx| {
                    let bytes = cmx.to_bytes();
                    pallas::Base::from_repr(bytes)
                        .expect("orchard note commitment is a valid pallas::Base")
                })
                .collect::<Vec<_>>()
        })
    }

    /// Access the ironwood note commitments from all transactions in this block,
    /// as `pallas::Base` values for the note commitment tree.
    pub fn ironwood_note_commitments(&self) -> impl Iterator<Item = pallas::Base> + '_ {
        self.transactions.iter().flat_map(|transaction| {
            transaction
                .ironwood_note_commitments()
                .map(|cmx| {
                    let bytes = cmx.to_bytes();
                    pallas::Base::from_repr(bytes)
                        .expect("ironwood note commitment is a valid pallas::Base")
                })
                .collect::<Vec<_>>()
        })
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

    /// Count how many Ironwood transactions exist in a block,
    /// i.e. transactions where the Ironwood bundle is non-empty (NU6.3 onward).
    pub fn ironwood_transactions_count(&self) -> u64 {
        self.transactions
            .iter()
            .filter(|tx| tx.has_ironwood_shielded_data())
            .count()
            .try_into()
            .expect("number of transactions must fit u64")
    }

    /// Returns the overall chain value pool change in this block---the negative sum of the
    /// transaction value balances in this block.
    ///
    /// These are the changes in the transparent, Sprout, Sapling, Orchard, Ironwood and
    /// Deferred chain value pools, and in the NSM reserve, as a result of this block.
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
    /// `previous_value_pools` must be the exact parent's balances. Reserve-funded coinbase and
    /// funding outputs are validated against that context once NSM reissuance starts.
    /// Genesis transparent outputs are permanently unspendable, so they do not enter the pool.
    /// Custom networks with nonzero genesis outputs must rebuild state created before this rule;
    /// public-network genesis outputs are zero, so their historical balances are unchanged.
    ///
    /// Note that the chain value pool has the opposite sign to the transaction value pool.
    pub fn chain_value_pool_change(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
        deferred_pool_balance_change: DeferredPoolBalanceChange,
        network: &Network,
        previous_value_pools: ValueBalance<NonNegative>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        // `Result<T, E>` implements `IntoIterator`, so a `flat_map(|t| t.value_balance(utxos))`
        // would silently drop transactions whose value balance returns `Err`. Use `try_fold`
        // to propagate the first error instead.
        //
        // The transaction fees are accumulated in the same pass, because the NSM reserve
        // contribution is calculated from them: `value_balance()` walks every input's UTXO, so a
        // second pass would double the work on the state's block commit path.
        //
        // The fees are only accumulated once NU7 is active, so that this stays byte-for-byte the
        // same calculation as before NU7 everywhere else. A transaction's fee is its remaining
        // value, which is only guaranteed to be non-negative for semantically verified
        // transactions, and this method is also called on blocks that have not been through the
        // transaction verifier.
        let needs_fees = self
            .coinbase_height()
            .is_some_and(|height| NetworkUpgrade::current(network, height) >= NetworkUpgrade::Nu7);

        let (tx_pool_sum, transaction_fees) = self.transactions.iter().try_fold(
            (
                ValueBalance::<NegativeAllowed>::zero(),
                Amount::<NonNegative>::zero(),
            ),
            |(pool_sum, fees), tx| {
                let value_balance = tx.value_balance(utxos)?;

                // The coinbase transaction consumes the fees rather than paying them, so it is
                // excluded from the total, exactly as in the block verifier's miner fee sum.
                let fees = if needs_fees && !tx.is_coinbase() {
                    let fee = value_balance
                        .remaining_transaction_value()
                        .map_err(ValueBalanceError::Total)?;

                    (fees + fee).map_err(ValueBalanceError::Total)?
                } else {
                    fees
                };

                Ok::<_, ValueBalanceError>(((pool_sum + value_balance)?, fees))
            },
        )?;

        let height = self.coinbase_height().ok_or(ValueBalanceError::Subsidy(
            subsidy::SubsidyError::NoCoinbase,
        ))?;
        let previous_reserve = previous_value_pools.nsm_reserve_amount();
        let additional = subsidy::nsm_subsidy(height, network, previous_reserve)
            .map_err(ValueBalanceError::NsmReserve)?;
        let contribution = subsidy::nsm_fee_contribution(height, network, transaction_fees)
            .map_err(ValueBalanceError::NsmReserve)?;
        let nsm_reserve_change = contribution
            .checked_sub(additional)
            .expect("the difference of two nonnegative amounts fits a signed amount");

        // These checks need the exact parent, not a best-tip estimate. Running them here uses
        // the existing ordered contextual commit/proposal path, without waiting for a parent
        // inside semantic verification or blocking the state writer.
        if network
            .nsm_reissuance_height()
            .is_some_and(|start| height >= start)
        {
            let total = subsidy::block_subsidy(height, network, previous_reserve)
                .map_err(ValueBalanceError::Subsidy)?;
            let deferred = subsidy::subsidy_is_valid(self, network, total)
                .map_err(ValueBalanceError::Subsidy)?;
            subsidy::miner_fees_are_valid(
                self.transactions.first().ok_or(ValueBalanceError::Subsidy(
                    subsidy::SubsidyError::NoCoinbase,
                ))?,
                height,
                transaction_fees,
                total,
                deferred,
                network,
            )
            .map_err(ValueBalanceError::Subsidy)?;
            if deferred != deferred_pool_balance_change {
                return Err(ValueBalanceError::Subsidy(
                    subsidy::SubsidyError::InvalidMinerFees,
                ));
            }
        }

        let mut chain_value_pool_change = tx_pool_sum.neg();
        chain_value_pool_change.set_deferred_amount(deferred_pool_balance_change.value());
        chain_value_pool_change.set_nsm_reserve_amount(nsm_reserve_change);
        if height.is_min() {
            chain_value_pool_change.set_transparent_value_balance(ValueBalance::zero());
        }

        if NetworkUpgrade::Nu7
            .activation_height(network)
            .is_some_and(|activation| height + 1 == Some(activation))
        {
            let seeded = previous_value_pools
                .add_chain_value_pool_change(chain_value_pool_change)?
                .with_nsm_reserve_seed(height, network)?;
            chain_value_pool_change.set_nsm_reserve_amount(
                seeded
                    .nsm_reserve_amount()
                    .checked_sub(previous_reserve)
                    .expect("the difference of two nonnegative amounts fits a signed amount"),
            );
        }

        Ok(chain_value_pool_change)
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
