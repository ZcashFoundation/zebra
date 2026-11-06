//! Content-addressed synchronization artifacts.

use std::{fmt, str::FromStr};

use hex::FromHex;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
mod tests;

/// The SHA-256 hash of a content-addressed synchronization artifact: a
/// snapshot manifest or piece, as served by `get-object` requests.
///
/// Displayed, parsed, and serialized as 64 lowercase hex digits, in byte order.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct ObjectHash(pub [u8; 32]);

impl ObjectHash {
    /// Returns the hash of `object`'s complete contents.
    pub fn of(object: &[u8]) -> Self {
        ObjectHash(Sha256::digest(object).into())
    }
}

impl fmt::Display for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for ObjectHash {
    type Err = hex::FromHexError;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        <[u8; 32]>::from_hex(hex).map(ObjectHash)
    }
}

impl From<[u8; 32]> for ObjectHash {
    fn from(bytes: [u8; 32]) -> Self {
        ObjectHash(bytes)
    }
}

impl Serialize for ObjectHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ObjectHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        hex.parse().map_err(de::Error::custom)
    }
}
