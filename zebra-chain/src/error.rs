//! Errors that can occur inside any `zebra-chain` submodule.
use thiserror::Error;

use crate::{amount, block::error::BlockError};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Errors related to random bytes generation.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum RandError {
    /// Error of the `try_fill_bytes` function.
    #[error("failed to generate a secure stream of random bytes")]
    FillBytes,
}

/// An error type pertaining to shielded notes.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum NoteError {
    /// Errors of type `RandError`.
    #[error("Randomness generation failure")]
    InsufficientRandomness(#[from] RandError),
    /// Error of `pallas::Point::from_bytes()` for new rho randomness.
    #[error("failed to generate an Orchard note's rho.")]
    InvalidRho,
}

/// An error type pertaining to note commitments.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum NoteCommitmentError {
    /// Errors of type `RandError`.
    #[error("Randomness generation failure")]
    InsufficientRandomness(#[from] RandError),
    /// Error of `jubjub::AffinePoint::try_from`.
    #[error("failed to generate a sapling::NoteCommitment from a diversifier")]
    InvalidDiversifier,
}

/// An error type pertaining to key generation, parsing, modification,
/// randomization.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// Errors of type `RandError`.
    #[error("Randomness generation failure")]
    InsufficientRandomness(#[from] RandError),
}

/// An error type pertaining to payment address generation, parsing,
/// modification, diversification.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// Errors of type `RandError`.
    #[error("Randomness generation failure")]
    InsufficientRandomness(#[from] RandError),
    /// Errors pertaining to diversifier generation.
    #[error("Randomness did not hash into the Jubjub group for producing a new diversifier")]
    DiversifierGenerationFailure,
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("funding stream expected output not found")]
    FundingStreamNotFound,

    #[error("miner fees are invalid")]
    InvalidMinerFees,

    #[error("a sum of amounts overflowed")]
    SumOverflow,

    #[error("unsupported height")]
    UnsupportedHeight,

    #[error("invalid amount")]
    InvalidAmount(amount::Error),

    #[error("invalid burn amount")]
    InvalidBurnAmount,
}

impl From<amount::Error> for SubsidyError {
    fn from(amount: amount::Error) -> Self {
        Self::InvalidAmount(amount)
    }
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(missing_docs)]
pub enum CoinbaseTransactionError {
    #[error("first transaction must be coinbase")]
    Position,

    #[error("coinbase input found in non-coinbase transaction")]
    AfterFirst,

    #[error("coinbase transaction MUST NOT have any JoinSplit descriptions")]
    HasJoinSplit,

    #[error("coinbase transaction MUST NOT have any Spend descriptions")]
    HasSpend,

    #[error("coinbase transaction MUST NOT have any Output descriptions pre-Heartwood")]
    HasOutputPreHeartwood,

    #[error("coinbase transaction MUST NOT have the EnableSpendsOrchard flag set")]
    HasEnableSpendsOrchard,

    #[error("coinbase transaction Sapling or Orchard outputs MUST be decryptable with an all-zero outgoing viewing key")]
    OutputsNotDecryptable,

    #[error("coinbase inputs MUST NOT exist in mempool")]
    InMempool,

    #[error(
        "coinbase expiry {expiry_height:?} must be the same as the block {block_height:?} \
         after NU5 activation, failing transaction: {transaction_hash:?}"
    )]
    ExpiryBlockHeight {
        expiry_height: Option<crate::block::Height>,
        block_height: crate::block::Height,
        transaction_hash: crate::transaction::Hash,
    },

    #[error("coinbase transaction failed subsidy validation")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Subsidy(#[from] SubsidyError),

    #[error("TODO")]
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    Block(#[from] BlockError),
}
