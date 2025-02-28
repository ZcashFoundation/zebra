//! Errors that can occur when checking Block consensus rules.

use thiserror::Error;

use crate::error::SubsidyError;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[derive(Clone, Error, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(missing_docs)]
pub enum BlockError {
    #[error("block has no transactions")]
    NoTransactions,

    #[error("transaction has wrong consensus branch id for block network upgrade")]
    WrongTransactionConsensusBranchId,

    #[error("block failed subsidy validation")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Subsidy(#[from] SubsidyError),
}
