//! Block and transaction serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use zebra_chain::{
    block::{self, Height},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, Transaction},
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
#[cfg(any(test, feature = "proptest-impl"))]
use serde::{Deserialize, Serialize};

/// The maximum valid block height supported by the on-disk encoding.
///
/// The encoding can represent every `u32`, but verified blocks never exceed [`Height::MAX`].
pub const MAX_ON_DISK_HEIGHT: Height = Height::MAX;

/// The number of bytes in the variable-width [`Height`] length prefix.
pub const HEIGHT_LENGTH_PREFIX_DISK_BYTES: usize = 1;

/// [`TransactionIndex`]es are stored as 2 bytes on disk.
///
/// This reduces database size and increases lookup performance.
pub const TX_INDEX_DISK_BYTES: usize = 2;

// Block and transaction types

/// A transaction's index in its block.
///
/// # Consensus
///
/// A 2-byte index supports on-disk storage of transactions in blocks up to ~5 MB.
/// (The current maximum block size is 2 MB.)
///
/// Since Zebra only stores fully verified blocks on disk,
/// blocks larger than this size are rejected before reaching the database.
///
/// (The maximum transaction count is tested by the large generated block serialization tests.)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Default, Serialize, Deserialize)
)]
pub struct TransactionIndex(pub(super) u16);

impl TransactionIndex {
    /// Creates a transaction index from the inner type.
    pub fn from_index(transaction_index: u16) -> TransactionIndex {
        TransactionIndex(transaction_index)
    }

    /// Returns this index as the inner type.
    pub fn index(&self) -> u16 {
        self.0
    }

    /// Creates a transaction index from a `usize`.
    pub fn from_usize(transaction_index: usize) -> TransactionIndex {
        TransactionIndex(
            transaction_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Returns this index as a `usize`.
    pub fn as_usize(&self) -> usize {
        self.0.into()
    }

    /// Creates a transaction index from a `u64`.
    pub fn from_u64(transaction_index: u64) -> TransactionIndex {
        TransactionIndex(
            transaction_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Returns this index as a `u64`.
    #[allow(dead_code)]
    pub fn as_u64(&self) -> u64 {
        self.0.into()
    }

    /// The minimum value of a transaction index.
    ///
    /// This value corresponds to the coinbase transaction.
    pub const MIN: Self = Self(u16::MIN);

    /// The maximum value of a transaction index.
    ///
    /// This value corresponds to the highest possible transaction index.
    pub const MAX: Self = Self(u16::MAX);
}

/// A transaction's location in the chain, by block height and transaction index.
///
/// This provides a chain-order list of transactions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Default, Serialize, Deserialize)
)]
pub struct TransactionLocation {
    /// The block height of the transaction.
    pub height: Height,

    /// The index of the transaction in its block.
    pub index: TransactionIndex,
}

impl TransactionLocation {
    /// Creates a transaction location from a block height and transaction index.
    pub fn from_parts(height: Height, index: TransactionIndex) -> TransactionLocation {
        TransactionLocation { height, index }
    }

    /// Creates a transaction location from a block height and transaction index.
    pub fn from_index(height: Height, transaction_index: u16) -> TransactionLocation {
        TransactionLocation {
            height,
            index: TransactionIndex::from_index(transaction_index),
        }
    }

    /// Creates a transaction location from a block height and `usize` transaction index.
    pub fn from_usize(height: Height, transaction_index: usize) -> TransactionLocation {
        TransactionLocation {
            height,
            index: TransactionIndex::from_usize(transaction_index),
        }
    }

    /// Creates a transaction location from a block height and `u64` transaction index.
    pub fn from_u64(height: Height, transaction_index: u64) -> TransactionLocation {
        TransactionLocation {
            height,
            index: TransactionIndex::from_u64(transaction_index),
        }
    }

    /// The minimum value of a transaction location.
    ///
    /// This value corresponds to the genesis coinbase transaction.
    pub const MIN: Self = Self {
        height: Height::MIN,
        index: TransactionIndex::MIN,
    };

    /// The maximum value of a transaction location.
    ///
    /// This value corresponds to the last transaction in the highest possible block.
    pub const MAX: Self = Self {
        height: Height::MAX,
        index: TransactionIndex::MAX,
    };

    /// The minimum value of a transaction location for `height`.
    ///
    /// This value is the coinbase transaction.
    pub const fn min_for_height(height: Height) -> Self {
        Self {
            height,
            index: TransactionIndex::MIN,
        }
    }

    /// The maximum value of a transaction location for `height`.
    ///
    /// This value can be a valid entry, but it won't fit in a 2MB block.
    pub const fn max_for_height(height: Height) -> Self {
        Self {
            height,
            index: TransactionIndex::MAX,
        }
    }
}

// Block and transaction trait impls

impl IntoDisk for block::Header {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for block::Header {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bytes
            .as_ref()
            .zcash_deserialize_into()
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

/// Decodes one canonical height from the start of `disk_bytes`, returning the remaining bytes.
///
/// Zero has a zero length prefix and no payload. Every other height has a 1–4 byte length
/// prefix followed by its minimal big-endian representation. The length prefix makes the
/// encoding self-delimiting and preserves numeric ordering in RocksDB's lexicographic key order.
pub(crate) fn height_from_bytes_prefix(disk_bytes: &[u8]) -> Option<(Height, &[u8])> {
    let (&byte_len, rest) = disk_bytes.split_first()?;
    let byte_len = usize::from(byte_len);

    if byte_len == 0 {
        return Some((Height::MIN, rest));
    }
    if byte_len > size_of::<u32>() || rest.len() < byte_len {
        return None;
    }

    let (height_bytes, remaining) = rest.split_at(byte_len);
    if height_bytes[0] == 0 {
        return None;
    }

    let mut mem_bytes = [0; size_of::<u32>()];
    mem_bytes[size_of::<u32>() - byte_len..].copy_from_slice(height_bytes);

    Some((Height(u32::from_be_bytes(mem_bytes)), remaining))
}

impl IntoDisk for Height {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.0.to_be_bytes();
        let Some(first_nonzero) = mem_bytes.iter().position(|&byte| byte != 0) else {
            return vec![0];
        };
        let height_bytes = &mem_bytes[first_nonzero..];
        let mut disk_bytes =
            Vec::with_capacity(HEIGHT_LENGTH_PREFIX_DISK_BYTES + height_bytes.len());
        disk_bytes.push(
            height_bytes
                .len()
                .try_into()
                .expect("a u32 has at most four significant bytes"),
        );
        disk_bytes.extend_from_slice(height_bytes);
        disk_bytes
    }
}

impl FromDisk for Height {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (height, remaining) = height_from_bytes_prefix(disk_bytes.as_ref())
            .expect("stored heights must use their canonical variable-width encoding");
        assert!(
            remaining.is_empty(),
            "stored standalone heights must not have trailing bytes"
        );
        height
    }
}

impl IntoDisk for block::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
}

impl FromDisk for block::Hash {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Self(array)
    }
}

// Transaction trait impls

impl IntoDisk for Transaction {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for Transaction {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes.as_ref();

        // TODO: skip cryptography verification during transaction deserialization from storage,
        //       or do it in a rayon thread (ideally in parallel with other transactions)
        bytes
            .as_ref()
            .zcash_deserialize_into()
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

/// TransactionIndex is only serialized as part of TransactionLocation
impl IntoDisk for TransactionIndex {
    type Bytes = [u8; TX_INDEX_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        self.index().to_be_bytes()
    }
}

impl FromDisk for TransactionIndex {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let disk_bytes = disk_bytes.as_ref().try_into().unwrap();

        TransactionIndex::from_index(u16::from_be_bytes(disk_bytes))
    }
}

impl TransactionLocation {
    /// Decodes a transaction location from the start of `disk_bytes`, returning the remaining bytes.
    pub(crate) fn from_bytes_prefix(disk_bytes: &[u8]) -> Option<(Self, &[u8])> {
        let (height, rest) = height_from_bytes_prefix(disk_bytes)?;
        let (index_bytes, remaining) = rest.split_at_checked(TX_INDEX_DISK_BYTES)?;
        let index = TransactionIndex::from_bytes(index_bytes);

        Some((TransactionLocation { height, index }, remaining))
    }
}

impl IntoDisk for TransactionLocation {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        [self.height.as_bytes(), self.index.as_bytes().to_vec()].concat()
    }
}

impl FromDisk for Option<TransactionLocation> {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (location, remaining) = TransactionLocation::from_bytes_prefix(disk_bytes.as_ref())?;
        remaining.is_empty().then_some(location)
    }
}

impl FromDisk for TransactionLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (location, remaining) = TransactionLocation::from_bytes_prefix(disk_bytes.as_ref())
            .expect("stored transaction locations must use their canonical encoding");
        assert!(
            remaining.is_empty(),
            "stored standalone transaction locations must not have trailing bytes"
        );
        location
    }
}

impl IntoDisk for transaction::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
}

impl FromDisk for transaction::Hash {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        transaction::Hash(disk_bytes.as_ref().try_into().unwrap())
    }
}
