//! Block and transaction serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use zebra_chain::{
    block::{self, Height},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, Transaction},
};

use crate::service::finalized_state::disk_format::{
    expand_zero_be_bytes, truncate_zero_be_bytes, FromDisk, IntoDisk,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// The maximum value of an on-disk serialized [`Height`].
///
/// This allows us to store [`OutputIndex`]es in 8 bytes,
/// which makes database searches more efficient.
///
/// # Consensus
///
/// This maximum height supports on-disk storage of blocks until around 2050.
///
/// Since Zebra only stores fully verified blocks on disk, blocks with heights
/// larger than this height are rejected before reaching the database.
/// (It would take decades to generate a valid chain this high.)
pub const MAX_ON_DISK_HEIGHT: Height = Height((1 << (HEIGHT_DISK_BYTES * 8)) - 1);

/// [`Height`]s are stored as 3 bytes on disk.
///
/// This reduces database size and increases lookup performance.
pub const HEIGHT_DISK_BYTES: usize = 3;

/// [`TransactionIndex`]es are stored as 2 bytes on disk.
///
/// This reduces database size and increases lookup performance.
pub const TX_INDEX_DISK_BYTES: usize = 2;

// Transaction types

/// A transaction's index in its block.
///
/// # Consensus
///
/// This maximum height supports on-disk storage of transactions in blocks up to ~5 MB.
/// (The current maximum block size is 2 MB.)
///
/// Since Zebra only stores fully verified blocks on disk,
/// blocks larger than this size are rejected before reaching the database.
///
/// (The maximum transaction count is tested by the large generated block serialization tests.)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TransactionIndex(u16);

impl TransactionIndex {
    /// Creates a transaction index from the native index integer type.
    pub fn from_usize(transaction_index: usize) -> TransactionIndex {
        TransactionIndex(
            transaction_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Returns this index as the native index integer type.
    #[allow(dead_code)]
    pub fn as_usize(&self) -> usize {
        self.0
            .try_into()
            .expect("the maximum valid index fits in usize")
    }
}

/// A transaction's location in the chain, by block height and transaction index.
///
/// This provides a chain-order list of transactions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TransactionLocation {
    /// The block height of the transaction.
    pub height: Height,

    /// The index of the transaction in its block.
    pub index: TransactionIndex,
}

impl TransactionLocation {
    /// Creates a transaction location from a block height and index (as the native index integer type).
    pub fn from_usize(height: Height, transaction_index: usize) -> TransactionLocation {
        TransactionLocation {
            height,
            index: TransactionIndex::from_usize(transaction_index),
        }
    }
}

// Block trait impls

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

impl IntoDisk for Height {
    /// Consensus: see the note at [`MAX_ON_DISK_HEIGHT`].
    type Bytes = [u8; HEIGHT_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.0.to_be_bytes();

        let disk_bytes = truncate_zero_be_bytes(&mem_bytes, HEIGHT_DISK_BYTES);

        disk_bytes.try_into().unwrap()
    }
}

impl FromDisk for Height {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let mem_len = u32::BITS / 8;
        let mem_len = mem_len.try_into().unwrap();

        let mem_bytes = expand_zero_be_bytes(bytes.as_ref(), mem_len);
        Height(u32::from_be_bytes(mem_bytes.try_into().unwrap()))
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
        self.0.to_be_bytes()
    }
}

impl FromDisk for TransactionIndex {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        TransactionIndex(u16::from_be_bytes(disk_bytes.as_ref().try_into().unwrap()))
    }
}

impl IntoDisk for TransactionLocation {
    type Bytes = [u8; HEIGHT_DISK_BYTES + TX_INDEX_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let index_bytes = self.index.as_bytes().to_vec();

        [height_bytes, index_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for TransactionLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (height_bytes, index_bytes) = disk_bytes.as_ref().split_at(HEIGHT_DISK_BYTES);

        let height = Height::from_bytes(height_bytes);
        let index = TransactionIndex::from_bytes(index_bytes);

        TransactionLocation { height, index }
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
