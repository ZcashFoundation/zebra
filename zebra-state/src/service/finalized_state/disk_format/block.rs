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

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

// Transaction types

/// A transaction's location in the chain, by block height and transaction index.
///
/// This provides a chain-order list of transactions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct TransactionLocation {
    /// The block height of the transaction.
    pub height: Height,

    /// The index of the transaction in its block.
    pub index: u32,
}

impl TransactionLocation {
    /// Create a transaction location from a block height and index (as the native index integer type).
    #[allow(dead_code)]
    pub fn from_usize(height: Height, index: usize) -> TransactionLocation {
        TransactionLocation {
            height,
            index: index
                .try_into()
                .expect("all valid indexes are much lower than u32::MAX"),
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
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
}

impl FromDisk for Height {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Height(u32::from_be_bytes(array))
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

impl IntoDisk for TransactionLocation {
    type Bytes = [u8; 8];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes();
        let index_bytes = self.index.to_be_bytes();

        [height_bytes, index_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for TransactionLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (height_bytes, index_bytes) = disk_bytes.as_ref().split_at(4);

        let height = Height::from_bytes(height_bytes);
        let index = u32::from_be_bytes(index_bytes.try_into().unwrap());

        TransactionLocation { height, index }
    }
}

impl IntoDisk for transaction::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
}
