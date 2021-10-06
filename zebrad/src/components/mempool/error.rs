//! Errors that can occur when manipulating transactions in the mempool.

use thiserror::Error;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[derive(Error, Clone, Debug, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum MempoolError {
    #[error("transaction already exists in mempool")]
    InMempool,

    #[error("transaction did not pass consensus validation")]
    Invalid(#[from] zebra_consensus::error::TransactionError),

    #[error("transaction is committed in block {0:?}")]
    InBlock(zebra_chain::block::Hash),

    #[error("transaction has expired")]
    Expired,

    #[error("transaction fee is too low for the current mempool state")]
    LowFee,

    #[error("transaction was not found in mempool")]
    NotInMempool,

    #[error("transaction evicted from the mempool due to size restrictions")]
    Excess,

    #[error("transaction is in the mempool rejected list")]
    Rejected,

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

    /// The transaction has a spend conflict with another transaction already in the mempool.
    #[error(
        "transaction rejected because another transaction in the mempool has already spent some of \
        its inputs"
    )]
    SpendConflict,

    #[error("mempool is disabled since synchronization is behind the chain tip")]
    Disabled,

    #[error("error calling a service")]
    ServiceError,
}
