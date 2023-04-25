//! Errors that can occur inside any `zebra-chain` submodule.
use thiserror::Error;

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
