//! Tests for the finalized disk format.

use serde::{Deserialize, Serialize};

mod prop;
mod snapshot;

/// A formatting struct for raw key-value data
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
struct KV {
    /// The raw key bytes, as a hexadecimal-encoded string.
    k: String,

    /// The raw value bytes, as a hexadecimal-encoded string.
    v: String,
}

impl KV {
    /// Create a new `KV` from raw key-value data.
    fn new<K, V>(key: K, value: V) -> KV
    where
        K: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        KV::new_hex(hex::encode(key), hex::encode(value))
    }

    /// Create a new `KV` from hex-encoded key-value data.
    fn new_hex(key: String, value: String) -> KV {
        KV { k: key, v: value }
    }
}
