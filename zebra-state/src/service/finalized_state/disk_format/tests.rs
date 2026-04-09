//! Tests for the finalized disk format.

#![allow(clippy::unwrap_in_result)]

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod prop;
#[cfg(test)]
mod snapshot;

/// A formatting struct for raw key-value data
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct KV {
    /// The raw key bytes, as a hexadecimal-encoded string.
    k: String,

    /// The raw value bytes, as a hexadecimal-encoded string.
    v: String,
}

impl KV {
    /// Create a new `KV` from raw key-value data.
    pub fn new<K, V>(key: K, value: V) -> KV
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        KV::new_hex(hex::encode(key), hex::encode(value))
    }

    /// Create a new `KV` from hex-encoded key-value data.
    pub fn new_hex(key: String, value: String) -> KV {
        KV { k: key, v: value }
    }
}
