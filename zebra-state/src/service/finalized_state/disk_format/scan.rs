//! Serialization formats for the shielded scanner results database.
//!
//! Due to Rust's orphan rule, these serializations must be implemented in this crate.
//!
//! # Correctness
//!
//! Once format versions are implemented for the scanner database,
//! `zebra_scan::Storage::database_format_version_in_code()` must be incremented
//! each time the database format (column, serialization, etc) changes.

use zebra_chain::{block::Height, transaction};

use crate::{FromDisk, IntoDisk};

use super::block::HEIGHT_DISK_BYTES;

/// The fixed length of the scanning result.
///
/// TODO: If the scanning result doesn't have a fixed length, either:
/// - deserialize using internal length or end markers,
/// - prefix it with a length, or
/// - stop storing vectors of results on disk, instead store each result with a unique key.
pub const SAPLING_SCANNING_RESULT_LENGTH: usize = 32;

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
pub type SaplingScanningKey = String;

/// The type used in Zebra to store Sapling scanning results.
pub type SaplingScannedResult = transaction::Hash;

/// A database column family entry for a block scanned with a Sapling vieweing key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaplingScannedDatabaseEntry {
    /// The database column family key. Must be unique for each scanning key and scanned block.
    pub index: SaplingScannedDatabaseIndex,

    /// The database column family value.
    pub value: SaplingScannedResult,
}

/// A database column family key for a block scanned with a Sapling vieweing key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaplingScannedDatabaseIndex {
    /// The Sapling viewing key used to scan the block.
    pub sapling_key: SaplingScanningKey,

    /// The height of the block.
    pub height: Height,
}

impl SaplingScannedDatabaseIndex {
    /// The minimum value of a sapling scanned database index.
    /// This value is guarateed to be the minimum, and not correspond to a valid key.
    pub const fn min() -> Self {
        Self {
            // The empty string is the minimum value in RocksDB lexicographic order.
            sapling_key: String::new(),
            // Genesis is the minimum height.
            height: Height(0),
        }
    }
}

impl IntoDisk for SaplingScanningKey {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        SaplingScanningKey::as_bytes(self).to_vec()
    }
}

impl FromDisk for SaplingScanningKey {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        SaplingScanningKey::from_utf8(bytes.as_ref().to_vec())
            .expect("only valid UTF-8 strings are written to the database")
    }
}

impl IntoDisk for SaplingScannedDatabaseIndex {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = Vec::new();

        bytes.extend(self.sapling_key.as_bytes());
        bytes.extend(self.height.as_bytes());

        bytes
    }
}

impl FromDisk for SaplingScannedDatabaseIndex {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        let (sapling_key, height) = bytes.split_at(bytes.len() - HEIGHT_DISK_BYTES);

        Self {
            sapling_key: SaplingScanningKey::from_bytes(sapling_key),
            height: Height::from_bytes(height),
        }
    }
}

impl IntoDisk for Vec<SaplingScannedResult> {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.iter()
            .flat_map(SaplingScannedResult::as_bytes)
            .collect()
    }
}

impl FromDisk for Vec<SaplingScannedResult> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bytes
            .as_ref()
            .chunks(SAPLING_SCANNING_RESULT_LENGTH)
            .map(SaplingScannedResult::from_bytes)
            .collect()
    }
}
