//! Block and transaction serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use zebra_chain::{
    block::{self, Block, Height},
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction,
};

use crate::service::finalized_state::disk_format::{
    expand_zero_be_bytes, truncate_zero_be_bytes, FromDisk, IntoDisk, IntoDiskFixedLen,
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
#[allow(dead_code)]
pub const MAX_ON_DISK_HEIGHT: Height = Height(Height::MAX.0 / 256);

// Transaction types

/// A transaction's index in its block.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TransactionIndex(u32);

impl TransactionIndex {
    /// Create a transaction index from the native index integer type.
    #[allow(dead_code)]
    pub fn from_usize(transaction_index: usize) -> TransactionIndex {
        TransactionIndex(
            transaction_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Return this index as the native index integer type.
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
    /// Create a transaction location from a block height and index (as the native index integer type).
    #[allow(dead_code)]
    pub fn from_usize(height: Height, transaction_index: usize) -> TransactionLocation {
        TransactionLocation {
            height,
            index: TransactionIndex::from_usize(transaction_index),
        }
    }
}

// Block trait impls

impl IntoDisk for Block {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for Block {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Block::zcash_deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

impl IntoDisk for Height {
    /// Consensus: see the note at [`MAX_ON_DISK_HEIGHT`].
    type Bytes = [u8; 3];

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.0.to_be_bytes();

        let disk_bytes = truncate_zero_be_bytes(&mem_bytes, Height::fixed_disk_byte_len());

        disk_bytes.try_into().unwrap()
    }
}

impl FromDisk for Height {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let mem_bytes = expand_zero_be_bytes(bytes.as_ref(), 4);
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

impl IntoDisk for TransactionIndex {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
}

impl FromDisk for TransactionIndex {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        TransactionIndex(u32::from_be_bytes(disk_bytes.as_ref().try_into().unwrap()))
    }
}

impl IntoDisk for TransactionLocation {
    type Bytes = [u8; 7];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let index_bytes = self.index.as_bytes().to_vec();

        [height_bytes, index_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for TransactionLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let height_len = Height::fixed_disk_byte_len();

        let (height_bytes, index_bytes) = disk_bytes.as_ref().split_at(height_len);

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
