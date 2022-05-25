//! Errors that can occur when checking Block consensus rules.

use thiserror::Error;

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, PartialEq, Eq)]
pub enum BlockError {
    #[error("transaction has wrong consensus branch id for block network upgrade")]
    WrongTransactionConsensusBranchId,
}
