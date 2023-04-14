//! Unmined Zcash transaction identifiers and transactions.
//!
//! Transaction version 5 is uniquely identified by [`WtxId`] when unmined, and
//! [`struct@Hash`] in the blockchain. The effects of a v5 transaction
//! (spends and outputs) are uniquely identified by the same
//! [`struct@Hash`] in both cases.
//!
//! Transaction versions 1-4 are uniquely identified by legacy
//! [`struct@Hash`] transaction IDs, whether they have been mined or not.
//! So Zebra, and the Zcash network protocol, don't use witnessed transaction
//! IDs for them.
//!
//! Zebra's [`UnminedTxId`] and [`UnminedTx`] enums provide the correct unique
//! ID for unmined transactions. They can be used to handle transactions
//! regardless of version, and get the [`WtxId`] or [`struct@Hash`] when
//! required.

use std::{fmt, sync::Arc};

use crate::{
    amount::{Amount, NonNegative},
    serialization::ZcashSerialize,
    transaction::{
        AuthDigest, Hash,
        Transaction::{self, *},
        WtxId,
    },
};

use UnminedTxId::*;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

// Documentation-only
#[allow(unused_imports)]
use crate::block::MAX_BLOCK_BYTES;

mod zip317;

/// The minimum cost value for a transaction in the mempool.
///
/// Contributes to the randomized, weighted eviction of transactions from the
/// mempool when it reaches a max size, also based on the total cost.
///
/// > Each transaction has a cost, which is an integer defined as:
/// >
/// > max(serialized transaction size in bytes, 4000)
/// >
/// > The threshold 4000 for the cost function is chosen so that the size in bytes
/// > of a typical fully shielded Sapling transaction (with, say, 2 shielded outputs
/// > and up to 5 shielded inputs) will fall below the threshold. This has the effect
/// > of ensuring that such transactions are not evicted preferentially to typical
/// > transparent transactions because of their size.
///
/// [ZIP-401]: https://zips.z.cash/zip-0401
const MEMPOOL_TRANSACTION_COST_THRESHOLD: u64 = 4000;

/// When a transaction pays a fee less than the conventional fee,
/// this low fee penalty is added to its cost for mempool eviction.
///
/// See [VerifiedUnminedTx::eviction_weight()] for details.
const MEMPOOL_TRANSACTION_LOW_FEE_PENALTY: u64 = 16_000;

/// A unique identifier for an unmined transaction, regardless of version.
///
/// "The transaction ID of a version 4 or earlier transaction is the SHA-256d hash
/// of the transaction encoding in the pre-v5 format described above.
///
/// The transaction ID of a version 5 transaction is as defined in [ZIP-244].
///
/// A v5 transaction also has a wtxid (used for example in the peer-to-peer protocol)
/// as defined in [ZIP-239]."
/// [Spec: Transaction Identifiers]
///
/// [ZIP-239]: https://zips.z.cash/zip-0239
/// [ZIP-244]: https://zips.z.cash/zip-0244
/// [Spec: Transaction Identifiers]: https://zips.z.cash/protocol/protocol.pdf#txnidentifiers
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum UnminedTxId {
    /// A legacy unmined transaction identifier.
    ///
    /// Used to uniquely identify unmined version 1-4 transactions.
    /// (After v1-4 transactions are mined, they can be uniquely identified
    /// using the same [`struct@Hash`].)
    Legacy(Hash),

    /// A witnessed unmined transaction identifier.
    ///
    /// Used to uniquely identify unmined version 5 transactions.
    /// (After v5 transactions are mined, they can be uniquely identified
    /// using only the [`struct@Hash`] in their `WtxId.id`.)
    ///
    /// For more details, see [`WtxId`].
    Witnessed(WtxId),
}

impl From<Transaction> for UnminedTxId {
    fn from(transaction: Transaction) -> Self {
        // use the ref implementation, to avoid cloning the transaction
        UnminedTxId::from(&transaction)
    }
}

impl From<&Transaction> for UnminedTxId {
    fn from(transaction: &Transaction) -> Self {
        match transaction {
            V1 { .. } | V2 { .. } | V3 { .. } | V4 { .. } => Legacy(transaction.into()),
            V5 { .. } => Witnessed(transaction.into()),
        }
    }
}

impl From<Arc<Transaction>> for UnminedTxId {
    fn from(transaction: Arc<Transaction>) -> Self {
        transaction.as_ref().into()
    }
}

impl From<WtxId> for UnminedTxId {
    fn from(wtx_id: WtxId) -> Self {
        Witnessed(wtx_id)
    }
}

impl From<&WtxId> for UnminedTxId {
    fn from(wtx_id: &WtxId) -> Self {
        (*wtx_id).into()
    }
}

impl fmt::Display for UnminedTxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Legacy(hash) => hash.fmt(f),
            Witnessed(id) => id.fmt(f),
        }
    }
}

impl UnminedTxId {
    /// Create a new [`UnminedTxId`] using a v1-v4 legacy transaction ID.
    ///
    /// # Correctness
    ///
    /// This method must only be used for v1-v4 transaction IDs.
    /// [`struct@Hash`] does not uniquely identify unmined v5
    /// transactions.
    pub fn from_legacy_id(legacy_tx_id: Hash) -> UnminedTxId {
        Legacy(legacy_tx_id)
    }

    /// Return the unique ID that will be used if this transaction gets mined into a block.
    ///
    /// # Correctness
    ///
    /// For v1-v4 transactions, this method returns an ID which changes
    /// if this transaction's effects (spends and outputs) change, or
    /// if its authorizing data changes (signatures, proofs, and scripts).
    ///
    /// But for v5 transactions, this ID uniquely identifies the transaction's effects.
    pub fn mined_id(&self) -> Hash {
        match self {
            Legacy(legacy_id) => *legacy_id,
            Witnessed(wtx_id) => wtx_id.id,
        }
    }

    /// Returns a mutable reference to the unique ID
    /// that will be used if this transaction gets mined into a block.
    ///
    /// See [`Self::mined_id`] for details.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn mined_id_mut(&mut self) -> &mut Hash {
        match self {
            Legacy(legacy_id) => legacy_id,
            Witnessed(wtx_id) => &mut wtx_id.id,
        }
    }

    /// Return the digest of this transaction's authorizing data,
    /// (signatures, proofs, and scripts), if it is a v5 transaction.
    pub fn auth_digest(&self) -> Option<AuthDigest> {
        match self {
            Legacy(_) => None,
            Witnessed(wtx_id) => Some(wtx_id.auth_digest),
        }
    }

    /// Returns a mutable reference to the digest of this transaction's authorizing data,
    /// (signatures, proofs, and scripts), if it is a v5 transaction.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn auth_digest_mut(&mut self) -> Option<&mut AuthDigest> {
        match self {
            Legacy(_) => None,
            Witnessed(wtx_id) => Some(&mut wtx_id.auth_digest),
        }
    }
}

/// An unmined transaction, and its pre-calculated unique identifying ID.
///
/// This transaction has been structurally verified.
/// (But it might still need semantic or contextual verification.)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnminedTx {
    /// The unmined transaction itself.
    pub transaction: Arc<Transaction>,

    /// A unique identifier for this unmined transaction.
    pub id: UnminedTxId,

    /// The size in bytes of the serialized transaction data
    pub size: usize,

    /// The conventional fee for this transaction, as defined by [ZIP-317].
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#fee-calculation
    pub conventional_fee: Amount<NonNegative>,
}

impl fmt::Display for UnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnminedTx")
            .field("transaction", &self.transaction.to_string())
            .field("serialized_size", &self.size)
            .field("conventional_fee", &self.conventional_fee)
            .finish()
    }
}

// Each of these conversions is implemented slightly differently,
// to avoid cloning the transaction where possible.

impl From<Transaction> for UnminedTx {
    fn from(transaction: Transaction) -> Self {
        let size = transaction.zcash_serialized_size();
        let conventional_fee = zip317::conventional_fee(&transaction);

        // The borrow is actually needed to avoid taking ownership
        #[allow(clippy::needless_borrow)]
        Self {
            id: (&transaction).into(),
            size,
            conventional_fee,
            transaction: Arc::new(transaction),
        }
    }
}

impl From<&Transaction> for UnminedTx {
    fn from(transaction: &Transaction) -> Self {
        let size = transaction.zcash_serialized_size();
        let conventional_fee = zip317::conventional_fee(transaction);

        Self {
            id: transaction.into(),
            size,
            conventional_fee,
            transaction: Arc::new(transaction.clone()),
        }
    }
}

impl From<Arc<Transaction>> for UnminedTx {
    fn from(transaction: Arc<Transaction>) -> Self {
        let size = transaction.zcash_serialized_size();
        let conventional_fee = zip317::conventional_fee(&transaction);

        Self {
            id: transaction.as_ref().into(),
            size,
            conventional_fee,
            transaction,
        }
    }
}

impl From<&Arc<Transaction>> for UnminedTx {
    fn from(transaction: &Arc<Transaction>) -> Self {
        let size = transaction.zcash_serialized_size();
        let conventional_fee = zip317::conventional_fee(transaction);

        Self {
            id: transaction.as_ref().into(),
            size,
            conventional_fee,
            transaction: transaction.clone(),
        }
    }
}

/// A verified unmined transaction, and the corresponding transaction fee.
///
/// This transaction has been fully verified, in the context of the mempool.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct VerifiedUnminedTx {
    /// The unmined transaction.
    pub transaction: UnminedTx,

    /// The transaction fee for this unmined transaction.
    pub miner_fee: Amount<NonNegative>,

    /// The number of legacy signature operations in this transaction's
    /// transparent inputs and outputs.
    pub legacy_sigop_count: u64,

    /// The number of unpaid actions for `transaction`,
    /// as defined by [ZIP-317] for block production.
    ///
    /// The number of actions is limited by [`MAX_BLOCK_BYTES`], so it fits in a u32.
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
    pub unpaid_actions: u32,

    /// The fee weight ratio for `transaction`, as defined by [ZIP-317] for block production.
    ///
    /// This is not consensus-critical, so we use `f32` for efficient calculations
    /// when the mempool holds a large number of transactions.
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
    pub fee_weight_ratio: f32,
}

impl fmt::Display for VerifiedUnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedUnminedTx")
            .field("transaction", &self.transaction.to_string())
            .field("miner_fee", &self.miner_fee)
            .field("legacy_sigop_count", &self.legacy_sigop_count)
            .field("unpaid_actions", &self.unpaid_actions)
            .field("fee_weight_ratio", &self.fee_weight_ratio)
            .finish()
    }
}

impl VerifiedUnminedTx {
    /// Create a new verified unmined transaction from an unmined transaction,
    /// its miner fee, and its legacy sigop count.
    pub fn new(
        transaction: UnminedTx,
        miner_fee: Amount<NonNegative>,
        legacy_sigop_count: u64,
    ) -> Self {
        let fee_weight_ratio = zip317::conventional_fee_weight_ratio(&transaction, miner_fee);
        let unpaid_actions = zip317::unpaid_actions(&transaction, miner_fee);

        Self {
            transaction,
            miner_fee,
            legacy_sigop_count,
            fee_weight_ratio,
            unpaid_actions,
        }
    }

    /// Returns `true` if the transaction pays at least the [ZIP-317] conventional fee.
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#mempool-size-limiting
    pub fn pays_conventional_fee(&self) -> bool {
        self.miner_fee >= self.transaction.conventional_fee
    }

    /// The cost in bytes of the transaction, as defined in [ZIP-401].
    ///
    /// A reflection of the work done by the network in processing them (proof
    /// and signature verification; networking overheads; size of in-memory data
    /// structures).
    ///
    /// > Each transaction has a cost, which is an integer defined as:
    /// >
    /// > max(serialized transaction size in bytes, 4000)
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    pub fn cost(&self) -> u64 {
        std::cmp::max(
            u64::try_from(self.transaction.size).expect("fits in u64"),
            MEMPOOL_TRANSACTION_COST_THRESHOLD,
        )
    }

    /// The computed _eviction weight_ of a verified unmined transaction as part
    /// of the mempool set, as defined in [ZIP-317] and [ZIP-401].
    ///
    /// Standard rule:
    ///
    /// > Each transaction also has an eviction weight, which is cost +
    /// > low_fee_penalty, where low_fee_penalty is 16000 if the transaction pays
    /// > a fee less than the conventional fee, otherwise 0.
    ///
    /// > zcashd and zebrad limit the size of the mempool as described in [ZIP-401].
    /// > This specifies a low fee penalty that is added to the "eviction weight" if the transaction
    /// > pays a fee less than the conventional transaction fee. This threshold is
    /// > modified to use the new conventional fee formula.
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#mempool-size-limiting
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    pub fn eviction_weight(&self) -> u64 {
        let mut cost = self.cost();

        if !self.pays_conventional_fee() {
            cost += MEMPOOL_TRANSACTION_LOW_FEE_PENALTY
        }

        cost
    }
}
