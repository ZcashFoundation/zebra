//! Serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{collections::BTreeMap, convert::TryInto, sync::Arc};

use bincode::Options;

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parameters::Network,
    primitives::zcash_history,
    sapling,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    sprout, transaction, transparent,
    value_balance::ValueBalance,
};

pub use types::TransactionLocation;

pub mod types;

#[cfg(test)]
mod tests;

/// Helper trait for defining the exact format used to interact with disk per
/// type.
pub trait IntoDisk {
    /// The type used to compare a value as a key to other keys stored in a
    /// database.
    type Bytes: AsRef<[u8]>;

    /// Converts the current type to its disk format in `zs_get()`,
    /// without necessarily allocating a new ivec.
    fn as_bytes(&self) -> Self::Bytes;
}

impl<'a, T> IntoDisk for &'a T
where
    T: IntoDisk,
{
    type Bytes = T::Bytes;

    fn as_bytes(&self) -> Self::Bytes {
        T::as_bytes(*self)
    }
}

impl<T> IntoDisk for Arc<T>
where
    T: IntoDisk,
{
    type Bytes = T::Bytes;

    fn as_bytes(&self) -> Self::Bytes {
        T::as_bytes(&*self)
    }
}

/// Helper type for retrieving types from the disk with the correct format.
///
/// The ivec should be correctly encoded by IntoDisk.
pub trait FromDisk: Sized {
    /// Function to convert the disk bytes back into the deserialized type.
    ///
    /// # Panics
    ///
    /// - if the input data doesn't deserialize correctly
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self;
}

impl<T> FromDisk for Arc<T>
where
    T: FromDisk,
{
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Arc::new(T::from_bytes(bytes))
    }
}

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

impl IntoDisk for TransactionLocation {
    type Bytes = [u8; 8];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.0.to_be_bytes();
        let index_bytes = self.index.to_be_bytes();

        let mut bytes = [0; 8];

        bytes[0..4].copy_from_slice(&height_bytes);
        bytes[4..8].copy_from_slice(&index_bytes);

        bytes
    }
}

impl FromDisk for TransactionLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let disk_bytes = disk_bytes.as_ref();
        let height = {
            let mut bytes = [0; 4];
            bytes.copy_from_slice(&disk_bytes[0..4]);
            let height = u32::from_be_bytes(bytes);
            Height(height)
        };

        let index = {
            let mut bytes = [0; 4];
            bytes.copy_from_slice(&disk_bytes[4..8]);
            u32::from_be_bytes(bytes)
        };

        TransactionLocation { height, index }
    }
}

impl IntoDisk for transaction::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
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

impl IntoDisk for sprout::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
}

impl IntoDisk for sapling::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
}

impl IntoDisk for orchard::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        let nullifier: orchard::Nullifier = *self;
        nullifier.into()
    }
}

impl IntoDisk for () {
    type Bytes = [u8; 0];

    fn as_bytes(&self) -> Self::Bytes {
        []
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

impl IntoDisk for transparent::Utxo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let mut bytes = vec![0; 5];
        bytes[0..4].copy_from_slice(&self.height.0.to_be_bytes());
        bytes[4] = self.from_coinbase as u8;
        self.output
            .zcash_serialize(&mut bytes)
            .expect("serialization to vec doesn't fail");
        bytes
    }
}

impl FromDisk for transparent::Utxo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let (meta_bytes, output_bytes) = bytes.as_ref().split_at(5);
        let height = Height(u32::from_be_bytes(meta_bytes[0..4].try_into().unwrap()));
        let from_coinbase = meta_bytes[4] == 1u8;
        let output = output_bytes
            .zcash_deserialize_into()
            .expect("db has serialized data");
        Self {
            output,
            height,
            from_coinbase,
        }
    }
}

impl IntoDisk for transparent::OutPoint {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }
}

impl IntoDisk for sprout::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl IntoDisk for sapling::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl IntoDisk for orchard::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl IntoDisk for ValueBalance<NonNegative> {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
    }
}

impl FromDisk for ValueBalance<NonNegative> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        ValueBalance::from_bytes(array).unwrap()
    }
}

// The following implementations for the note commitment trees use `serde` and
// `bincode` because currently the inner Merkle tree frontier (from
// `incrementalmerkletree`) only supports `serde` for serialization. `bincode`
// was chosen because it is small and fast. We explicitly use `DefaultOptions`
// in particular to disallow trailing bytes; see
// https://docs.rs/bincode/1.3.3/bincode/config/index.html#options-struct-vs-bincode-functions

impl IntoDisk for sprout::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for sprout::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}
impl IntoDisk for sapling::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for sapling::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

impl IntoDisk for orchard::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for orchard::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryTreeParts {
    network: Network,
    size: u32,
    peaks: BTreeMap<u32, zcash_history::Entry>,
    current_height: Height,
}

impl IntoDisk for NonEmptyHistoryTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let data = HistoryTreeParts {
            network: self.network(),
            size: self.size(),
            peaks: self.peaks().clone(),
            current_height: self.current_height(),
        };
        bincode::DefaultOptions::new()
            .serialize(&data)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for NonEmptyHistoryTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let parts: HistoryTreeParts = bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect(
                "deserialization format should match the serialization format used by IntoDisk",
            );
        NonEmptyHistoryTree::from_cache(
            parts.network,
            parts.size,
            parts.peaks,
            parts.current_height,
        )
        .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}
