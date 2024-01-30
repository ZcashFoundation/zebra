//! Defines types and implements methods for parsing Orchard viewing keys and converting them to `zebra-chain` types

use crate::parameters::Network;

/// A Zcash Orchard viewing key
#[derive(Debug, Clone)]
pub enum OrchardViewingKey {}

impl OrchardViewingKey {
    /// Accepts an encoded Orchard viewing key to decode
    ///
    /// Returns a [`OrchardViewingKey`] if successful, or None otherwise
    pub fn parse(_key: &str, _network: Network) -> Option<Self> {
        // TODO: parse Orchard viewing keys
        None
    }
}
