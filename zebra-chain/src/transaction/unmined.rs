//! Unmined Zcash transaction identifiers and transactions.
//!
//! Zebra's [`UnminedTxId`] and [`UnminedTx`] enums provide the correct ID for the
//! transaction version. They can be used to handle transactions regardless of version,
//! and get the [`WtxId`] or [`Hash`] when required.
//!
//! Transaction versions 1-4 are uniquely identified by narrow transaction IDs,
//! so Zebra and the Zcash network protocol don't use wide transaction IDs for them.

use std::sync::Arc;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::{
    Hash,
    Transaction::{self, *},
    WtxId,
};

use UnminedTxId::*;

/// A unique identifier for an unmined transaction, regardless of version.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum UnminedTxId {
    /// A narrow unmined transaction identifier.
    ///
    /// Used to uniquely identify transaction versions 1-4.
    Narrow(Hash),

    /// A wide unmined transaction identifier.
    ///
    /// Used to uniquely identify transaction version 5.
    Wide(WtxId),
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
            V1 { .. } | V2 { .. } | V3 { .. } | V4 { .. } => Narrow(transaction.into()),
            V5 { .. } => Wide(transaction.into()),
        }
    }
}

impl From<WtxId> for UnminedTxId {
    fn from(wtx_id: WtxId) -> Self {
        Wide(wtx_id)
    }
}

impl From<&WtxId> for UnminedTxId {
    fn from(wtx_id: &WtxId) -> Self {
        (*wtx_id).into()
    }
}

impl UnminedTxId {
    /// Return a new `UnminedTxId` using a v1-v4 legacy transaction ID.
    ///
    /// # Correctness
    ///
    /// This method must only be used for v1-v4 transaction IDs.
    /// [`Hash`] does not uniquely identify unmined v5 transactions.
    #[allow(dead_code)]
    pub fn from_legacy_id(legacy_tx_id: Hash) -> UnminedTxId {
        Narrow(legacy_tx_id)
    }
}

/// An unmined transaction, and its pre-calculated unique identifying ID.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct UnminedTx {
    /// A unique identifier for this unmined transaction.
    pub id: UnminedTxId,

    /// The unmined transaction itself.
    pub transaction: Arc<Transaction>,
}

// Each of these conversions is implemented slightly differently,
// to avoid cloning the transaction where possible.

impl From<Transaction> for UnminedTx {
    fn from(transaction: Transaction) -> Self {
        Self {
            id: (&transaction).into(),
            transaction: Arc::new(transaction),
        }
    }
}

impl From<&Transaction> for UnminedTx {
    fn from(transaction: &Transaction) -> Self {
        Self {
            id: transaction.into(),
            transaction: Arc::new(transaction.clone()),
        }
    }
}

impl From<Arc<Transaction>> for UnminedTx {
    fn from(transaction: Arc<Transaction>) -> Self {
        Self {
            id: transaction.as_ref().into(),
            transaction,
        }
    }
}

impl From<&Arc<Transaction>> for UnminedTx {
    fn from(transaction: &Arc<Transaction>) -> Self {
        Self {
            id: transaction.as_ref().into(),
            transaction: transaction.clone(),
        }
    }
}
