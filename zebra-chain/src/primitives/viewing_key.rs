//! Type definitions for viewing keys and their hashes.

use std::fmt;

use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};

use crate::parameters::Network;

pub mod sapling;

#[cfg(test)]
mod tests;

const KEY_HASH_BYTE_SIZE: usize = 32;

/// A Zcash Sapling or Orchard viewing key
// TODO: Add Orchard types and any other Sapling key types
#[derive(Debug, Clone)]
pub enum ViewingKey {
    /// An incoming viewing key for Sapling
    SaplingIvk(Box<SaplingIvk>),

    /// A diversifiable full viewing key for Sapling
    DiversifiableFullViewingKey(Box<DiversifiableFullViewingKey>),
}

impl ViewingKey {
    /// Returns an encoded byte representation of the viewing key
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::SaplingIvk(ivk) => ivk.to_repr().to_vec(),
            Self::DiversifiableFullViewingKey(dfvk) => dfvk.to_bytes().to_vec(),
        }
    }

    /// Parses an encoded viewing key and returns it as a [`ViewingKey`] type.
    pub fn parse(
        key: &str,
        network: Network,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync + 'static>> // TODO: Consider using an explicit error type
    {
        Self::parse_extended_full_viewing_key(key, network)
            .map(Self::DiversifiableFullViewingKey)
            // TODO: Call .or_else() with every other parse method, they won't do anything unless the HRP is correct
            .map_err(|err| err.to_string().into())
    }
}

impl From<DiversifiableFullViewingKey> for ViewingKey {
    fn from(value: DiversifiableFullViewingKey) -> Self {
        Self::DiversifiableFullViewingKey(Box::new(value))
    }
}

/// The hash of a viewing key for use as an identifier and for authorizing remote access
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
