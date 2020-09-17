//! Consensus validation errors

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TransactionError {
    #[error("no transactions")]
    NoTransactions,

    #[error("first transaction must be coinbase")]
    CoinbasePosition,

    #[error("coinbase input found in non-coinbase transaction")]
    CoinbaseInputFound,
}

#[derive(Error, Debug)]
pub enum BlockError {
    #[error("invalid transaction")]
    Transaction(#[from] TransactionError),

    #[error("block {0} is already in the chain at depth {1:?}")]
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

    #[error("block {0:?} failed the difficulty filter: hash {1:?} must be less than or equal to the difficulty threshold {2:?}")]
    DifficultyFilter(
        zebra_chain::block::Height,
        zebra_chain::block::Hash,
        zebra_chain::work::difficulty::ExpandedDifficulty,
    ),
}
