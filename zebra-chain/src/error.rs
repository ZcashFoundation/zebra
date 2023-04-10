//! Errors that can occur inside any `zebra-chain` submodule.
use thiserror::Error;

/// Number of times a `diversify_hash` will try to obtain a diversified base point.
pub const DIVERSIFY_HASH_TRIES: u8 = 5;

/// Errors related to random bytes generation.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum RandError {
    /// Error of the `try_fill_bytes` function.
    #[error("failed to generate a secure stream of random bytes")]
    FillBytes,
    /// Error of the `diversify_hash` function.
    #[error(
        "failed to generate a diversified base point after {} tries",
        DIVERSIFY_HASH_TRIES
    )]
    DiversifyHash,
}

/// Errors related to `AffinePoint`.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum AffinePointError {
    /// Error of `jubjub::AffinePoint::try_from`.
    #[error("failed to generate a AffinePoint from a diversifier")]
    FromDiversifier,
}

/// An error type that can have `RandError` or `AffineError`.
#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// Errors of type `RandError`.
    #[error("Random generation failure")]
    Rand(#[from] RandError),
    /// Errors of type `AffinePointError`.
    #[error("Affine point failure")]
    Affine(#[from] AffinePointError),
}
