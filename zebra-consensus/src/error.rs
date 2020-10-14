//! Consensus validation errors

use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("founders reward output not found")]
    FoundersRewardNotFound,
}

#[derive(Error, Debug, PartialEq)]
pub enum TransactionError {
    #[error("first transaction must be coinbase")]
    CoinbasePosition,

    #[error("coinbase input found in non-coinbase transaction")]
    CoinbaseInputFound,

    #[error("coinbase transaction failed subsidy validation")]
    Subsidy(#[from] SubsidyError),
}

impl From<SubsidyError> for BlockError {
    fn from(err: SubsidyError) -> BlockError {
        BlockError::Transaction(TransactionError::Subsidy(err))
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum BlockError {
    #[error("block contains invalid transactions")]
    Transaction(#[from] TransactionError),

    #[error("block haves no transactions")]
    NoTransactions,

    #[error("block {0:?} is already in the chain at depth {1:?}")]
    AlreadyInChain(zebra_chain::block::Hash, u32),

    #[error("invalid block {0:?}: missing block height")]
    MissingHeight(zebra_chain::block::Hash),

    #[error("invalid block height {0:?} in {1:?}: greater than the maximum height {2:?}")]
    MaxHeight(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::block::Height,
    ),

    #[error("invalid difficulty threshold in block header {0:?} {1:?}")]
    InvalidDifficulty(zebra_chain::block::Height, zebra_chain::block::Hash),

    #[error("block {0:?} has a difficulty threshold {2:?} that is easier than the {3:?} difficulty limit {4:?}, hash: {1:?}")]
    TargetDifficultyLimit(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::work::difficulty::ExpandedDifficulty,
        zebra_chain::parameters::Network,
        zebra_chain::work::difficulty::ExpandedDifficulty,
    ),

    #[error("block {0:?} has a hash {1:?} that is easier than the difficulty threshold {2:?}")]
    DifficultyFilter(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::work::difficulty::ExpandedDifficulty,
    ),
}
