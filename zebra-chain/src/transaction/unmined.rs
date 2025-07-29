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
    block::Height,
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

pub mod zip317;

/// The minimum cost value for a transaction in the mempool.
///
/// Contributes to the randomized, weighted eviction of transactions from the
/// mempool when it reaches a max size, also based on the total cost.
///
/// # Standard Rule
///
/// > Each transaction has a cost, which is an integer defined as:
/// >
/// > max(memory size in bytes, 10000)
/// >
/// > The memory size is an estimate of the size that a transaction occupies in the
/// > memory of a node. It MAY be approximated as the serialized transaction size in
/// > bytes.
/// >
/// > ...
/// >
/// > The threshold 10000 for the cost function is chosen so that the size in bytes of
/// > a minimal fully shielded Orchard transaction with 2 shielded actions (having a
/// > serialized size of 9165 bytes) will fall below the threshold. This has the effect
/// > of ensuring that such transactions are not evicted preferentially to typical
/// > transparent or Sapling transactions because of their size.
///
/// [ZIP-401]: https://zips.z.cash/zip-0401
pub const MEMPOOL_TRANSACTION_COST_THRESHOLD: u64 = 10_000;

/// When a transaction pays a fee less than the conventional fee,
/// this low fee penalty is added to its cost for mempool eviction.
///
/// See [VerifiedUnminedTx::eviction_weight()] for details.
///
/// [ZIP-401]: https://zips.z.cash/zip-0401
const MEMPOOL_TRANSACTION_LOW_FEE_PENALTY: u64 = 40_000;

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
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
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

impl fmt::Debug for UnminedTxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Logging unmined transaction IDs can leak sensitive user information,
            // particularly when Zebra is being used as a `lightwalletd` backend.
            Self::Legacy(_hash) => f.debug_tuple("Legacy").field(&self.to_string()).finish(),
            Self::Witnessed(_id) => f.debug_tuple("Witnessed").field(&self.to_string()).finish(),
        }
    }
}

impl fmt::Display for UnminedTxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Legacy(_hash) => f
                .debug_tuple("transaction::Hash")
                .field(&"private")
                .finish(),
            Witnessed(_id) => f.debug_tuple("WtxId").field(&"private").finish(),
        }
    }
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
            #[cfg(feature = "tx_v6")]
            V6 { .. } => Witnessed(transaction.into()),
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
#[derive(Clone, Eq, PartialEq)]
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

impl fmt::Debug for UnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Logging unmined transactions can leak sensitive user information,
        // particularly when Zebra is being used as a `lightwalletd` backend.
        f.debug_tuple("UnminedTx").field(&"private").finish()
    }
}

impl fmt::Display for UnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UnminedTx").field(&"private").finish()
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
//
// This struct can't be `Eq`, because it contains a `f32`.
#[derive(Clone, PartialEq)]
pub struct VerifiedUnminedTx {
    /// The unmined transaction.
    pub transaction: UnminedTx,

    /// The transaction fee for this unmined transaction.
    pub miner_fee: Amount<NonNegative>,

    /// The number of legacy signature operations in this transaction's
    /// transparent inputs and outputs.
    pub sigops: u32,

    /// The number of conventional actions for `transaction`, as defined by [ZIP-317].
    ///
    /// The number of actions is limited by [`MAX_BLOCK_BYTES`], so it fits in a u32.
    ///
    /// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
    pub conventional_actions: u32,

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

    /// The time the transaction was added to the mempool, or None if it has not
    /// reached the mempool yet.
    pub time: Option<chrono::DateTime<chrono::Utc>>,

    /// The tip height when the transaction was added to the mempool, or None if
    /// it has not reached the mempool yet.
    pub height: Option<Height>,
}

impl fmt::Debug for VerifiedUnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Logging unmined transactions can leak sensitive user information,
        // particularly when Zebra is being used as a `lightwalletd` backend.
        f.debug_tuple("VerifiedUnminedTx")
            .field(&"private")
            .finish()
    }
}

impl fmt::Display for VerifiedUnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VerifiedUnminedTx")
            .field(&"private")
            .finish()
    }
}

impl VerifiedUnminedTx {
    /// Create a new verified unmined transaction from an unmined transaction,
    /// its miner fee, and its legacy sigop count.
    pub fn new(
        transaction: UnminedTx,
        miner_fee: Amount<NonNegative>,
        legacy_sigop_count: u32,
    ) -> Result<Self, zip317::Error> {
        let fee_weight_ratio = zip317::conventional_fee_weight_ratio(&transaction, miner_fee);
        let conventional_actions = zip317::conventional_actions(&transaction.transaction);
        let unpaid_actions = zip317::unpaid_actions(&transaction, miner_fee);

        zip317::mempool_checks(unpaid_actions, miner_fee, transaction.size)?;

        Ok(Self {
            transaction,
            miner_fee,
            sigops: legacy_sigop_count,
            fee_weight_ratio,
            conventional_actions,
            unpaid_actions,
            time: None,
            height: None,
        })
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
    /// > Each transaction has a cost, which is an integer defined as...
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
    /// # Standard Rule
    ///
    /// > Each transaction also has an *eviction weight*, which is *cost* + *low_fee_penalty*,
    /// > where *low_fee_penalty* is 40000 if the transaction pays a fee less than the
    /// > conventional fee, otherwise 0. The conventional fee is currently defined in
    /// > [ZIP-317].
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
