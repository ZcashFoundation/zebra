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

use crate::service::finalized_state::disk_format::{
    expand_zero_be_bytes, truncate_zero_be_bytes, FromDisk, IntoDisk,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
#[cfg(any(test, feature = "proptest-impl"))]
use serde::{Deserialize, Serialize};

/// The maximum value of an on-disk serialized [`Height`].
///
/// This allows us to store [`OutputLocation`](crate::OutputLocation)s in
/// 8 bytes, which makes database searches more efficient.
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

/// [`TransactionLocation`]s are stored as a 3 byte height and a 2 byte transaction index.
///
/// This reduces database size and increases lookup performance.
pub const TRANSACTION_LOCATION_DISK_BYTES: usize = HEIGHT_DISK_BYTES + TX_INDEX_DISK_BYTES;

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

impl IntoDisk for Height {
    /// Consensus: see the note at [`MAX_ON_DISK_HEIGHT`].
    type Bytes = [u8; HEIGHT_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.0.to_be_bytes();

        let disk_bytes = truncate_zero_be_bytes(&mem_bytes, HEIGHT_DISK_BYTES);

        match disk_bytes {
            Some(b) => b.try_into().unwrap(),

            // # Security
            //
            // The RPC method or state query was given a block height that is ridiculously high.
            // But to save space in database indexes, we don't support heights 2^24 and above.
            //
            // Instead we return the biggest valid database Height to the lookup code.
            // So RPC methods and queued block checks will return an error or None.
            //
            // At the current block production rate, these heights can't be inserted into the
            // database until at least 2050. (Blocks are verified in strict height order.)
            None => truncate_zero_be_bytes(&MAX_ON_DISK_HEIGHT.0.to_be_bytes(), HEIGHT_DISK_BYTES)
                .expect("max on disk height is valid")
                .try_into()
                .unwrap(),
        }
    }
}

impl FromDisk for Height {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        const MEM_LEN: usize = size_of::<u32>();

        let mem_bytes = expand_zero_be_bytes::<MEM_LEN>(disk_bytes.as_ref());
        Height(u32::from_be_bytes(mem_bytes))
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

        // Sapling types (ValueCommitment, EphemeralPublicKey, ValidatingKey) now use
        // lazy deserialization -- raw bytes are stored and expensive Jubjub curve point
        // decompression is deferred until first access via inner(). See issue #7939.
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

impl IntoDisk for TransactionLocation {
    type Bytes = [u8; TRANSACTION_LOCATION_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let index_bytes = self.index.as_bytes().to_vec();

        [height_bytes, index_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for Option<TransactionLocation> {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        if disk_bytes.as_ref().len() == TRANSACTION_LOCATION_DISK_BYTES {
            Some(TransactionLocation::from_bytes(disk_bytes))
        } else {
            None
        }
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
