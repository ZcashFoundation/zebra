//! Transparent transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use zebra_chain::{
    block::Height,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction, transparent,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk, IntoDiskFixedLen};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

// Transparent types

/// A transaction's index in its block.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct OutputIndex(u32);

impl OutputIndex {
    /// Create a transparent output index from the native index integer type.
    #[allow(dead_code)]
    pub fn from_usize(output_index: usize) -> OutputIndex {
        OutputIndex(
            output_index
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

    /// Create a transparent output index from the Zcash consensus integer type.
    pub fn from_zcash(output_index: u32) -> OutputIndex {
        OutputIndex(output_index)
    }

    /// Return this index as the Zcash consensus integer type.
    #[allow(dead_code)]
    pub fn as_zcash(&self) -> u32 {
        self.0
    }
}

/// A transparent output's location in the chain, by block height and transaction index.
///
/// TODO: provide a chain-order list of transactions (#3150)
///       derive Ord, PartialOrd (#3150)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct OutputLocation {
    /// The transaction hash.
    pub hash: transaction::Hash,

    /// The index of the transparent output in its transaction.
    pub index: OutputIndex,
}

impl OutputLocation {
    /// Create a transparent output location from a transaction hash and index
    /// (as the native index integer type).
    #[allow(dead_code)]
    pub fn from_usize(hash: transaction::Hash, output_index: usize) -> OutputLocation {
        OutputLocation {
            hash,
            index: OutputIndex::from_usize(output_index),
        }
    }

    /// Create a transparent output location from a [`transparent::OutPoint`].
    pub fn from_outpoint(outpoint: &transparent::OutPoint) -> OutputLocation {
        OutputLocation {
            hash: outpoint.hash,
            index: OutputIndex::from_zcash(outpoint.index),
        }
    }
}

// Transparent trait impls

// TODO: serialize the index into a smaller number of bytes (#3152)
//       serialize the index in big-endian order (#3150)
impl IntoDisk for OutputIndex {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_le_bytes()
    }
}

impl FromDisk for OutputIndex {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        OutputIndex(u32::from_le_bytes(disk_bytes.as_ref().try_into().unwrap()))
    }
}

impl IntoDisk for OutputLocation {
    type Bytes = [u8; 36];

    fn as_bytes(&self) -> Self::Bytes {
        let hash_bytes = self.hash.as_bytes().to_vec();
        let index_bytes = self.index.as_bytes().to_vec();

        [hash_bytes, index_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for OutputLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let hash_len = transaction::Hash::fixed_byte_len();

        let (hash_bytes, index_bytes) = disk_bytes.as_ref().split_at(hash_len);

        let hash = transaction::Hash::from_bytes(hash_bytes);
        let index = OutputIndex::from_bytes(index_bytes);

        OutputLocation { hash, index }
    }
}

// TODO: just serialize the Output, and derive the Utxo data from OutputLocation (#3151)
impl IntoDisk for transparent::Utxo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let coinbase_flag_bytes = [self.from_coinbase as u8].to_vec();
        let output_bytes = self
            .output
            .zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail");

        [height_bytes, coinbase_flag_bytes, output_bytes].concat()
    }
}

impl FromDisk for transparent::Utxo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let height_len = Height::fixed_byte_len();

        let (height_bytes, rest_bytes) = bytes.as_ref().split_at(height_len);
        let (coinbase_flag_bytes, output_bytes) = rest_bytes.split_at(1);

        let height = Height::from_bytes(height_bytes);
        let from_coinbase = coinbase_flag_bytes[0] == 1u8;
        let output = output_bytes
            .zcash_deserialize_into()
            .expect("db has valid serialized data");

        Self {
            output,
            height,
            from_coinbase,
        }
    }
}
