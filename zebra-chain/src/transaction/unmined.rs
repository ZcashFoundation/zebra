//! Unmined Zcash transaction identifiers and transactions.
//!
//! Transaction version 5 is uniquely identified by [`WtxId`] when unmined,
//! and [`Hash`] in the blockchain. The effects of a v5 transaction (spends and outputs)
//! are uniquely identified by the same [`Hash`] in both cases.
//!
//! Transaction versions 1-4 are uniquely identified by legacy [`Hash`] transaction IDs,
//! whether they have been mined or not. So Zebra, and the Zcash network protocol,
//! don't use witnessed transaction IDs for them.
//!
//! Zebra's [`UnminedTxId`] and [`UnminedTx`] enums provide the correct unique ID for
//! unmined transactions. They can be used to handle transactions regardless of version,
//! and get the [`WtxId`] or [`Hash`] when required.

use std::{fmt, sync::Arc};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::serialization::ZcashSerialize;

use super::{
    AuthDigest, Hash,
    Transaction::{self, *},
    WtxId,
};

use UnminedTxId::*;

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
    /// using the same [`transaction::Hash`].)
    Legacy(Hash),

    /// A witnessed unmined transaction identifier.
    ///
    /// Used to uniquely identify unmined version 5 transactions.
    /// (After v5 transactions are mined, they can be uniquely identified
    /// using only the [`transaction::Hash`] in their `WtxId.id`.)
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
    /// Create a new `UnminedTxId` using a v1-v4 legacy transaction ID.
    ///
    /// # Correctness
    ///
    /// This method must only be used for v1-v4 transaction IDs.
    /// [`Hash`] does not uniquely identify unmined v5 transactions.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn mined_id(&self) -> Hash {
        match self {
            Legacy(legacy_id) => *legacy_id,
            Witnessed(wtx_id) => wtx_id.id,
        }
    }

    /// Return the digest of this transaction's authorizing data,
    /// (signatures, proofs, and scripts), if it is a v5 transaction.
    #[allow(dead_code)]
    pub fn auth_digest(&self) -> Option<AuthDigest> {
        match self {
            Legacy(_) => None,
            Witnessed(wtx_id) => Some(wtx_id.auth_digest),
        }
    }
}

/// An unmined transaction, and its pre-calculated unique identifying ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnminedTx {
    /// A unique identifier for this unmined transaction.
    pub id: UnminedTxId,

    /// The unmined transaction itself.
    pub transaction: Arc<Transaction>,

    /// The size in bytes of the serialized transaction data
    pub size: usize,
}

// Each of these conversions is implemented slightly differently,
// to avoid cloning the transaction where possible.

impl From<Transaction> for UnminedTx {
    fn from(transaction: Transaction) -> Self {
        Self {
            id: (&transaction).into(),
            size: transaction
                .zcash_serialized_size()
                .expect("all transactions have a size"),
            transaction: Arc::new(transaction),
        }
    }
}

impl From<&Transaction> for UnminedTx {
    fn from(transaction: &Transaction) -> Self {
        Self {
            id: transaction.into(),
            transaction: Arc::new(transaction.clone()),
            size: transaction
                .zcash_serialized_size()
                .expect("all transactions have a size"),
        }
    }
}

impl From<Arc<Transaction>> for UnminedTx {
    fn from(transaction: Arc<Transaction>) -> Self {
        Self {
            id: transaction.as_ref().into(),
            size: transaction
                .zcash_serialized_size()
                .expect("all transactions have a size"),
            transaction,
        }
    }
}

impl From<&Arc<Transaction>> for UnminedTx {
    fn from(transaction: &Arc<Transaction>) -> Self {
        Self {
            id: transaction.as_ref().into(),
            transaction: transaction.clone(),
            size: transaction
                .zcash_serialized_size()
                .expect("all transactions have a size"),
        }
    }
}
