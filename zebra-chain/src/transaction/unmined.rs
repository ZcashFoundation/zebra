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

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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
    /// A unique identifier for this unmined transaction.
    pub id: UnminedTxId,

    /// The unmined transaction itself.
    pub transaction: Arc<Transaction>,

    /// The size in bytes of the serialized transaction data
    pub size: usize,
}

impl fmt::Display for UnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnminedTx")
            .field("transaction", &self.transaction)
            .field("serialized_size", &self.size)
            .finish()
    }
}

// Each of these conversions is implemented slightly differently,
// to avoid cloning the transaction where possible.

impl From<Transaction> for UnminedTx {
    fn from(transaction: Transaction) -> Self {
        let size = transaction.zcash_serialized_size().expect(
            "unexpected serialization failure: all structurally valid transactions have a size",
        );

        // The borrow is actually needed to avoid taking ownership
        #[allow(clippy::needless_borrow)]
        Self {
            id: (&transaction).into(),
            size,
            transaction: Arc::new(transaction),
        }
    }
}

impl From<&Transaction> for UnminedTx {
    fn from(transaction: &Transaction) -> Self {
        let size = transaction.zcash_serialized_size().expect(
            "unexpected serialization failure: all structurally valid transactions have a size",
        );

        Self {
            id: transaction.into(),
            transaction: Arc::new(transaction.clone()),
            size,
        }
    }
}

impl From<Arc<Transaction>> for UnminedTx {
    fn from(transaction: Arc<Transaction>) -> Self {
        let size = transaction.zcash_serialized_size().expect(
            "unexpected serialization failure: all structurally valid transactions have a size",
        );

        Self {
            id: transaction.as_ref().into(),
            transaction,
            size,
        }
    }
}

impl From<&Arc<Transaction>> for UnminedTx {
    fn from(transaction: &Arc<Transaction>) -> Self {
        let size = transaction.zcash_serialized_size().expect(
            "unexpected serialization failure: all structurally valid transactions have a size",
        );

        Self {
            id: transaction.as_ref().into(),
            transaction: transaction.clone(),
            size,
        }
    }
}

/// A verified unmined transaction, and the corresponding transaction fee.
///
/// This transaction has been fully verified, in the context of the mempool.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct VerifiedUnminedTx {
    /// The unmined transaction.
    pub transaction: UnminedTx,

    /// The transaction fee for this unmined transaction.
    pub miner_fee: Amount<NonNegative>,
}

impl fmt::Display for VerifiedUnminedTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedUnminedTx")
            .field("transaction", &self.transaction)
            .field("miner_fee", &self.miner_fee)
            .finish()
    }
}

impl VerifiedUnminedTx {
    /// Create a new verified unmined transaction from a transaction and its fee.
    pub fn new(transaction: UnminedTx, miner_fee: Amount<NonNegative>) -> Self {
        Self {
            transaction,
            miner_fee,
        }
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
            self.transaction.size as u64,
            MEMPOOL_TRANSACTION_COST_THRESHOLD,
        )
    }

    /// The computed _eviction weight_ of a verified unmined transaction as part
    /// of the mempool set.
    ///
    /// Consensus rule:
    ///
    /// > Each transaction also has an eviction weight, which is cost +
    /// > low_fee_penalty, where low_fee_penalty is 16000 if the transaction pays
    /// > a fee less than the conventional fee, otherwise 0. The conventional fee
    /// > is currently defined as 1000 zatoshis
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    pub fn eviction_weight(self) -> u64 {
        let conventional_fee = 1000;

        let low_fee_penalty = if u64::from(self.miner_fee) < conventional_fee {
            16_000
        } else {
            0
        };

        self.cost() + low_fee_penalty
    }
}
