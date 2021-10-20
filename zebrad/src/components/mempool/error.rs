//! Errors that can occur when manipulating transactions in the mempool.

use thiserror::Error;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::storage::{
    ExactTipRejectionError, SameEffectsChainRejectionError, SameEffectsTipRejectionError,
};

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
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

    #[error("transaction is committed in block {0:?}")]
    InBlock(zebra_chain::block::Hash),

    #[error("transaction was not found in mempool")]
    NotInMempool,

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

    #[error("error calling a service")]
    ServiceError,
}
