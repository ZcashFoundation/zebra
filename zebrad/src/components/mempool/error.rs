//! Errors that can occur when interacting with the mempool.
//!
//! Most of the mempool errors are related to manipulating transactions in the
//! mempool.

use thiserror::Error;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::storage::{
    ExactTipRejectionError, SameEffectsChainRejectionError, SameEffectsTipRejectionError,
};

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum MempoolError {
    #[error("mempool storage has a cached tip rejection for this exact transaction")]
    StorageExactTip(#[from] ExactTipRejectionError),

    #[error(
        "mempool storage has a cached tip rejection for any transaction with the same effects"
    )]
    StorageEffectsTip(#[from] SameEffectsTipRejectionError),

    #[error(
        "mempool storage has a cached chain rejection for any transaction with the same effects"
    )]
    StorageEffectsChain(#[from] SameEffectsChainRejectionError),

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

    #[error("mempool is disabled since synchronization is behind the chain tip")]
    Disabled,
}
