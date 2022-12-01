//! Errors for redpallas operations.

use thiserror::Error;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Error {
    #[error("Malformed signing key encoding.")]
    MalformedSigningKey,
    #[error("Malformed verification key encoding.")]
    MalformedVerificationKey,
    #[error("Invalid signature.")]
    InvalidSignature,
}
