//! Type definitions for viewing keys and their hashes.

use std::fmt;

use crate::parameters::Network;

pub mod sapling;

use sapling::SaplingViewingKey;

pub mod orchard;

use orchard::OrchardViewingKey;

#[cfg(test)]
mod tests;

const KEY_HASH_BYTE_SIZE: usize = 32;

/// A Zcash Sapling or Orchard viewing key
// TODO: Add Orchard types and any other Sapling key types
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
        // TODO: Try types with prefixes first if some don't have prefixes?
        Self::parse_sapling(key, network).or_else(|| Self::parse_orchard(key, network))
    }
}

/// The hash of a viewing key for use as an identifier in zebra-scan, and
/// for authorizing remote access via the scanner's gRPC interface.
#[derive(Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct KeyHash([u8; KEY_HASH_BYTE_SIZE]);

impl From<&ViewingKey> for KeyHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        KeyHash::new(viewing_key)
    }
}

/// A [`ViewingKey`] and its [`KeyHash`]
pub struct ViewingKeyWithHash {
    /// The [`ViewingKey`]
    pub key: ViewingKey,

    /// The [`KeyHash`] for the above `key`
    pub hash: KeyHash,
}

impl From<ViewingKey> for ViewingKeyWithHash {
    fn from(key: ViewingKey) -> Self {
        Self {
            hash: KeyHash::new(&key),
            key,
        }
    }
}

impl From<&ViewingKey> for ViewingKeyWithHash {
    fn from(viewing_key: &ViewingKey) -> Self {
        viewing_key.clone().into()
    }
}

impl KeyHash {
    /// Creates a new [`KeyHash`] for the provided viewing key
    pub fn new(viewing_key: &ViewingKey) -> Self {
        Self(
            blake2b_simd::Params::new()
                .hash_length(KEY_HASH_BYTE_SIZE)
                .to_state()
                .update(&viewing_key.to_bytes())
                .finalize()
                .as_bytes()
                .try_into()
                .expect("hash length should be KEY_HASH_BYTE_SIZE bytes"),
        )
    }
}

impl fmt::Display for KeyHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl std::str::FromStr for KeyHash {
    type Err = bs58::decode::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut output = [0u8; KEY_HASH_BYTE_SIZE];
        bs58::decode(s).onto(&mut output)?;
        Ok(Self(output))
    }
}

impl From<SaplingViewingKey> for ViewingKey {
    fn from(key: SaplingViewingKey) -> Self {
        Self::Sapling(key)
    }
}

impl From<OrchardViewingKey> for ViewingKey {
    fn from(key: OrchardViewingKey) -> Self {
        Self::Orchard(key)
    }
}
