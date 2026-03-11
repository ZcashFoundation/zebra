//! Errors that can occur when interacting with the mempool.
//!
//! Most of the mempool errors are related to manipulating transactions in the
//! mempool.

use thiserror::Error;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::storage::{
    ExactTipRejectionError, NonStandardTransactionError, SameEffectsChainRejectionError,
    SameEffectsTipRejectionError,
};

/// Mempool errors.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum MempoolError {
    /// Transaction rejected based on its authorizing data (scripts, proofs,
    /// signatures). The rejection is valid for the current chain tip.
    ///
    /// See [`ExactTipRejectionError`] for more details.
    ///
    /// Note that the mempool caches this error. See [`super::storage::Storage`]
    /// for more details.
    #[error(
        "the transaction will be rejected from the mempool until the next chain tip block: {0}"
    )]
    StorageExactTip(#[from] ExactTipRejectionError),

    /// Transaction rejected based on its effects (spends, outputs, transaction
    /// header). The rejection is valid for the current chain tip.
    ///
    /// See [`SameEffectsTipRejectionError`] for more details.
    ///
    /// Note that the mempool caches this error. See [`super::storage::Storage`]
    /// for more details.
    #[error("any transaction with the same effects will be rejected from the mempool until the next chain tip block: {0}")]
    StorageEffectsTip(#[from] SameEffectsTipRejectionError),

    /// Transaction rejected based on its effects (spends, outputs, transaction
    /// header). The rejection is valid while the current chain continues to
    /// grow.
    ///
    /// See [`SameEffectsChainRejectionError`] for more details.
    ///
    /// Note that the mempool caches this error. See [`super::storage::Storage`]
    /// for more details.
    #[error("any transaction with the same effects will be rejected from the mempool until a chain reset: {0}")]
    StorageEffectsChain(#[from] SameEffectsChainRejectionError),

    /// Transaction rejected because the mempool already contains another
    /// transaction with the same hash.
    #[error("transaction already exists in mempool")]
    InMempool,

    /// The transaction hash is already queued, so this request was ignored.
    ///
    /// Another peer has already gossiped the same hash to us, or the mempool crawler has fetched it.
    #[error("transaction dropped because it is already queued for download")]
    AlreadyQueued,

    /// The queue is at capacity, so this request was ignored.
    ///
    /// The mempool crawler should discover this transaction later.
    /// If it is mined into a block, it will be downloaded by the syncer, or the inbound block downloader.
    ///
    /// The queue's capacity is [`super::downloads::MAX_INBOUND_CONCURRENCY`].
    #[error("transaction dropped because the queue is full")]
    FullQueue,

    /// The mempool is not enabled yet.
    ///
    /// Zebra enables the mempool when it is at the chain tip.
    #[error("mempool is disabled since synchronization is behind the chain tip")]
    Disabled,

    /// The transaction is non-standard.
    #[error("transaction is non-standard")]
    NonStandardTransaction(#[from] NonStandardTransactionError),
}
