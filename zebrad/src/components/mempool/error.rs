//! Errors that can occur when adding transactions to the mempool or querying it.

use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum TransactionInsertError {
    #[error("transaction already exists in mempool")]
    InMempool,

    #[error("transaction did not pass consensus validation")]
    TransactionInvalid(#[from] zebra_consensus::error::TransactionError),

    #[error("transaction is committed in  block {0:?}")]
    TransactionInBlock(zebra_chain::block::Hash),

    #[error("transaction has expired")]
    TransactionExpired,
}

#[derive(Error, Debug, PartialEq)]
pub enum TransactionQueryError {
    #[error("transaction was not found in mempool")]
    NotInMempool,
}
