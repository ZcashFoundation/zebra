//! Error types for zebra-chain parameters

use thiserror::Error;

/// An error indicating that Zebra network is not supported in type conversions.
#[derive(Clone, Debug, Error)]
#[error("Unsupported Zcash network parameters: {0}")]
pub struct UnsupportedNetwork(pub String);
