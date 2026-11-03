//! State snapshot commitments: the trust anchor of state-snapshot sync.
//!
//! A pruned node that starts from a state snapshot trusts the snapshot's
//! contents through a [`StateSnapshotCommitment`]: the height and block hash
//! the snapshot was taken at, and the hash of its manifest, which in turn
//! names every piece of the snapshot by hash. A commitment is pinned in this
//! crate for a network like its checkpoints are, or configured by the
//! operator.

use std::{fmt, str::FromStr};

use hex::FromHex;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{block, parameters::Network};

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

/// What a node trusts when it starts from a state snapshot: the snapshot's
/// height, the hash of the block at that height, and the hash of the manifest
/// that names the snapshot's pieces.
///
/// In a config file it is written as `{ height, hash, manifest }`, with the
/// hashes as hex strings.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSnapshotCommitment {
    /// The height the snapshot was taken at.
    pub height: block::Height,

    /// The hash of the block at [`Self::height`].
    #[serde(rename = "hash", with = "block_hash_hex")]
    pub block_hash: block::Hash,

    /// The hash of the snapshot manifest.
    pub manifest: ObjectHash,
}

impl Network {
    /// Returns the state snapshot commitment this build pins for the network,
    /// if any.
    ///
    /// A commitment is pinned per release, together with the checkpoint list
    /// it was cut with; no release has pinned one yet.
    pub fn state_snapshot_commitment(&self) -> Option<StateSnapshotCommitment> {
        None
    }
}

/// Serializes a block hash as its display-order hex string, as the
/// `block::Hash` `Display` and `FromStr` impls do.
mod block_hash_hex {
    use serde::{de, Deserialize, Deserializer, Serializer};

    use crate::block;

    pub fn serialize<S: Serializer>(hash: &block::Hash, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hash.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<block::Hash, D::Error> {
        let hex = String::deserialize(deserializer)?;
        hex.parse().map_err(de::Error::custom)
    }
}
