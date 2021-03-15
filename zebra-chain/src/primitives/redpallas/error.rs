use thiserror::Error;

#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
pub enum Error {
    #[error("Malformed signing key encoding.")]
    MalformedSigningKey,
    #[error("Malformed verification key encoding.")]
    MalformedVerificationKey,
    #[error("Invalid signature.")]
    InvalidSignature,
}
