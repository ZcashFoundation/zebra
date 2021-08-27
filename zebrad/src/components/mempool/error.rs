//! Errors that can occur when manipulating transactions in the mempool.

use thiserror::Error;

#[derive(Error, Clone, Debug, PartialEq)]
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
}
