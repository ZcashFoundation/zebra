//! Serialization formats for the shielded scanner results database.
//!
//! Due to Rust's orphan rule, these serializations must be implemented in this crate.
//!
//! # Correctness
//!
//! Once format versions are implemented for the scanner database,
//! `zebra_scan::Storage::database_format_version_in_code()` must be incremented
//! each time the database format (column, serialization, etc) changes.

use zebra_chain::transaction;

use crate::{FromDisk, IntoDisk};

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
pub type SaplingScanningKey = String;

/// The type used in Zebra to store Sapling scanning results.
pub type SaplingScannedResult = transaction::Hash;

/// The fixed length of the scanning result.
///
/// TODO: If the scanning result doesn't have a fixed length, either:
/// - deserialize using internal length or end markers,
/// - prefix it with a length, or
/// - stop storing vectors of results on disk, instead store each result with a unique key.
pub const SAPLING_SCANNING_RESULT_LENGTH: usize = 32;

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
