//! Serialization formats for the shielded scanner results database.
//!
//! Due to Rust's orphan rule, these serializations must be implemented in this crate.
//!
//! # Correctness
//!
//! `zebra_scan::Storage::database_format_version_in_code()` must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::fmt;

use hex::{FromHex, ToHex};
use zebra_chain::{block::Height, transaction};

use crate::{FromDisk, IntoDisk, TransactionLocation};

use super::block::TRANSACTION_LOCATION_DISK_BYTES;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
mod tests;

/// The type used in Zebra to store Sapling scanning keys.
/// It can represent a full viewing key or an individual viewing key.
pub type SaplingScanningKey = String;

/// Stores a scanning result.
///
/// Currently contains a TXID in "display order", which is big-endian byte order following the u256
/// convention set by Bitcoin and zcashd.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Default, serde::Serialize, serde::Deserialize)
)]
pub struct SaplingScannedResult(
    #[cfg_attr(any(test, feature = "proptest-impl"), serde(with = "hex"))] [u8; 32],
);

impl fmt::Display for SaplingScannedResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for SaplingScannedResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("SaplingScannedResult")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl ToHex for &SaplingScannedResult {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for SaplingScannedResult {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for SaplingScannedResult {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let result = <[u8; 32]>::from_hex(hex)?;

        Ok(Self::from_bytes_in_display_order(result))
    }
}

impl From<SaplingScannedResult> for transaction::Hash {
    fn from(scanned_result: SaplingScannedResult) -> Self {
        transaction::Hash::from_bytes_in_display_order(&scanned_result.0)
    }
}

impl From<transaction::Hash> for SaplingScannedResult {
    fn from(hash: transaction::Hash) -> Self {
        SaplingScannedResult(hash.bytes_in_display_order())
    }
}

impl SaplingScannedResult {
    /// Creates a `SaplingScannedResult` from bytes in display order.
    pub fn from_bytes_in_display_order(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the inner bytes in display order.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        self.0
    }
}

/// A database column family entry for a block scanned with a Sapling vieweing key.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary, Default))]
pub struct SaplingScannedDatabaseEntry {
    /// The database column family key. Must be unique for each scanning key and scanned block.
    pub index: SaplingScannedDatabaseIndex,

    /// The database column family value.
    pub value: Option<SaplingScannedResult>,
}

/// A database column family key for a block scanned with a Sapling vieweing key.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary, Default))]
pub struct SaplingScannedDatabaseIndex {
    /// The Sapling viewing key used to scan the block.
    pub sapling_key: SaplingScanningKey,

    /// The transaction location: block height and transaction index.
    pub tx_loc: TransactionLocation,
}

impl SaplingScannedDatabaseIndex {
    /// The minimum value of a sapling scanned database index.
    ///
    /// This value is guarateed to be the minimum, and not correspond to a valid key.
    //
    // Note: to calculate the maximum value, we need a key length.
    pub const fn min() -> Self {
        Self {
            // The empty string is the minimum value in RocksDB lexicographic order.
            sapling_key: String::new(),
            tx_loc: TransactionLocation::MIN,
        }
    }

    /// The minimum value of a sapling scanned database index for `sapling_key`.
    ///
    /// This value does not correspond to a valid entry.
    /// (The genesis coinbase transaction does not have shielded transfers.)
    pub fn min_for_key(sapling_key: &SaplingScanningKey) -> Self {
        Self {
            sapling_key: sapling_key.clone(),
            tx_loc: TransactionLocation::MIN,
        }
    }

    /// The maximum value of a sapling scanned database index for `sapling_key`.
    ///
    /// This value may correspond to a valid entry, but it won't be mined for many decades.
    pub fn max_for_key(sapling_key: &SaplingScanningKey) -> Self {
        Self {
            sapling_key: sapling_key.clone(),
            tx_loc: TransactionLocation::MAX,
        }
    }

    /// The minimum value of a sapling scanned database index for `sapling_key` and `height`.
    ///
    /// This value can be a valid entry for shielded coinbase.
    pub fn min_for_key_and_height(sapling_key: &SaplingScanningKey, height: Height) -> Self {
        Self {
            sapling_key: sapling_key.clone(),
            tx_loc: TransactionLocation::min_for_height(height),
        }
    }

    /// The maximum value of a sapling scanned database index for `sapling_key` and `height`.
    ///
    /// This value can be a valid entry, but it won't fit in a 2MB block.
    pub fn max_for_key_and_height(sapling_key: &SaplingScanningKey, height: Height) -> Self {
        Self {
            sapling_key: sapling_key.clone(),
            tx_loc: TransactionLocation::max_for_height(height),
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
        bytes.extend(self.tx_loc.as_bytes());

        bytes
    }
}

impl FromDisk for SaplingScannedDatabaseIndex {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        let (sapling_key, tx_loc) = bytes.split_at(bytes.len() - TRANSACTION_LOCATION_DISK_BYTES);

        Self {
            sapling_key: SaplingScanningKey::from_bytes(sapling_key),
            tx_loc: TransactionLocation::from_bytes(tx_loc),
        }
    }
}

// We can't implement IntoDisk or FromDisk for SaplingScannedResult,
// because the format is actually Option<SaplingScannedResult>.

impl IntoDisk for Option<SaplingScannedResult> {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = Vec::new();

        if let Some(result) = self.as_ref() {
            bytes.extend(result.bytes_in_display_order());
        }

        bytes
    }
}

impl FromDisk for Option<SaplingScannedResult> {
    #[allow(clippy::unwrap_in_result)]
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        if bytes.is_empty() {
            None
        } else {
            Some(SaplingScannedResult::from_bytes_in_display_order(
                bytes
                    .try_into()
                    .expect("unexpected incorrect SaplingScannedResult data length"),
            ))
        }
    }
}
