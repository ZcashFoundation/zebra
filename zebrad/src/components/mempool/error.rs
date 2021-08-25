//! Errors that can occur when adding transactions to the mempool or querying it.

use thiserror::Error;

#[derive(Error, Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum TransactionInsertError {
    #[error("transaction already exists in mempool")]
    InMempool,

    #[error("transaction did not pass consensus validation")]
    TransactionInvalid(#[from] zebra_consensus::error::TransactionError),

    #[error("transaction is committed in block {0:?}")]
    TransactionInBlock(zebra_chain::block::Hash),

    #[error("transaction has expired")]
    TransactionExpired,

    #[error("transaction fee is too low for the current mempool state")]
    LowFee,
}
