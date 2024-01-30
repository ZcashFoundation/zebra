//! Type definitions for viewing keys and their hashes.

use crate::parameters::Network;

mod orchard;
mod sapling;

use orchard::OrchardViewingKey;
use sapling::SaplingViewingKey;

#[cfg(test)]
mod tests;

/// A Zcash Sapling or Orchard viewing key
#[derive(Debug, Clone)]
pub enum ViewingKey {
    /// A viewing key for Sapling
    Sapling(SaplingViewingKey),

    /// A viewing key for Orchard
    Orchard(OrchardViewingKey),
}

impl ViewingKey {
    /// Returns an encoded byte representation of the viewing key
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Sapling(sapling_key) => sapling_key.to_bytes(),
            Self::Orchard(_) => vec![], // TODO: add Orchard keys
        }
    }

    /// Accepts an encoded Sapling viewing key to decode
    ///
    /// Returns a [`ViewingKey`] if successful, or None otherwise
    fn parse_sapling(sapling_key: &str, network: Network) -> Option<Self> {
        SaplingViewingKey::parse(sapling_key, network).map(Self::Sapling)
    }

    /// Accepts an encoded Orchard viewing key to decode
    ///
    /// Returns a [`ViewingKey`] if successful, or None otherwise
    fn parse_orchard(sapling_key: &str, network: Network) -> Option<Self> {
        OrchardViewingKey::parse(sapling_key, network).map(Self::Orchard)
    }

    /// Parses an encoded viewing key and returns it as a [`ViewingKey`] type.
    pub fn parse(key: &str, network: Network) -> Option<Self> {
        Self::parse_sapling(key, network).or_else(|| Self::parse_orchard(key, network))
    }
}
