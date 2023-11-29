//! Serialization formats for the shielded scanner results database.
//!
//! Due to Rust's orphan rule, these serializations must be implemented in this crate.
//!
//! # Correctness
//!
//! Once format versions are implemented for the scanner database,
//! `zebra_scan::Storage::database_format_version_in_code()` must be incremented
//! each time the database format (column, serialization, etc) changes.

use crate::{FromDisk, IntoDisk};

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
///
/// This must be the same type as `zebra_scan::storage::SaplingScanningKey`.
pub type SaplingScanningKey = String;

impl IntoDisk for SaplingScanningKey {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.as_bytes().to_vec()
    }
}

impl FromDisk for SaplingScanningKey {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        SaplingScanningKey::from_utf8(bytes.as_ref().to_vec())
            .expect("only valid UTF-8 strings are committed to the database")
    }
}
