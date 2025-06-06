//! Errors that can occur inside any `zebra-chain` submodule.

use std::{io, sync::Arc};
use thiserror::Error;

// TODO: Move all these enums into a common enum at the bottom.

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

/// `zebra-chain`'s errors
#[derive(Clone, Error, Debug)]
pub enum Error {
    /// Invalid consensus branch ID.
    #[error("invalid consensus branch id")]
    InvalidConsensusBranchId,

    /// Zebra's type could not be converted to its librustzcash equivalent.
    #[error("Zebra's type could not be converted to its librustzcash equivalent: {0}")]
    Conversion(#[from] Arc<io::Error>),

    /// The transaction is missing a network upgrade.
    #[error("the transaction is missing a network upgrade")]
    MissingNetworkUpgrade,
}

/// Allow converting `io::Error` to `Error`; we need this since we
/// use `Arc<io::Error>` in `Error::Conversion`.
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Arc::new(value).into()
    }
}

// We need to implement this manually because io::Error does not implement
// PartialEq.
impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match self {
            Error::InvalidConsensusBranchId => matches!(other, Error::InvalidConsensusBranchId),
            Error::Conversion(e) => {
                if let Error::Conversion(o) = other {
                    // Not perfect, but good enough for testing, which
                    // is the main purpose for our usage of PartialEq for errors
                    e.to_string() == o.to_string()
                } else {
                    false
                }
            }
            Error::MissingNetworkUpgrade => matches!(other, Error::MissingNetworkUpgrade),
        }
    }
}

impl Eq for Error {}
