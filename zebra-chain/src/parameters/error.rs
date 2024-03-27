//! Error types for zebra-chain parameters

use std::fmt;

/// An error indicating that Zebra network is not supported in type conversions.
#[derive(Debug)]
pub struct UnsupportedNetwork;

impl fmt::Display for UnsupportedNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported Zcash network parameters")
    }
}

impl std::error::Error for UnsupportedNetwork {}
