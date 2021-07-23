//! Module defining exactly how to move types in and out of rocksdb
use std::{convert::TryInto, fmt::Debug, sync::Arc};

use bincode::Options;
use zebra_chain::{
    block,
    block::Block,
    orchard, sapling,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    sprout, transaction, transparent,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransactionLocation {
    pub height: block::Height,
    pub index: u32,
}

// Helper trait for defining the exact format used to interact with disk per
// type.
pub trait IntoDisk {
    // The type used to compare a value as a key to other keys stored in a
    // database
    type Bytes: AsRef<[u8]>;

    // function to convert the current type to its disk format in `zs_get()`
    // without necessarily allocating a new IVec
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
            block::Height(height)
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

impl IntoDisk for block::Height {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
}

impl FromDisk for block::Height {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        block::Height(u32::from_be_bytes(array))
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
        let height = block::Height(u32::from_be_bytes(meta_bytes[0..4].try_into().unwrap()));
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

/// Helper trait for inserting (Key, Value) pairs into rocksdb with a consistently
/// defined format
pub trait DiskSerialize {
    /// Serialize and insert the given key and value into a rocksdb column family,
    /// overwriting any existing `value` for `key`.
    fn zs_insert<K, V>(&mut self, cf: &rocksdb::ColumnFamily, key: K, value: V)
    where
        K: IntoDisk + Debug,
        V: IntoDisk;

    /// Remove the given key form rocksdb column family if it exists.
    fn zs_delete<K>(&mut self, cf: &rocksdb::ColumnFamily, key: K)
    where
        K: IntoDisk + Debug;
}

impl DiskSerialize for rocksdb::WriteBatch {
    fn zs_insert<K, V>(&mut self, cf: &rocksdb::ColumnFamily, key: K, value: V)
    where
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        let key_bytes = key.as_bytes();
        let value_bytes = value.as_bytes();
        self.put_cf(cf, key_bytes, value_bytes);
    }

    fn zs_delete<K>(&mut self, cf: &rocksdb::ColumnFamily, key: K)
    where
        K: IntoDisk + Debug,
    {
        let key_bytes = key.as_bytes();
        self.delete_cf(cf, key_bytes);
    }
}

/// Helper trait for retrieving values from rocksdb column familys with a consistently
/// defined format
pub trait DiskDeserialize {
    /// Returns the value for `key` in the rocksdb column family `cf`, if present.
    fn zs_get<K, V>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk;

    /// Check if a rocksdb column family `cf` contains the serialized form of `key`.
    fn zs_contains<K>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> bool
    where
        K: IntoDisk;
}

impl DiskDeserialize for rocksdb::DB {
    fn zs_get<K, V>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we're going to deserialize it anyways, which avoids an
        // extra copy
        let value_bytes = self
            .get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur");

        value_bytes.map(V::from_bytes)
    }

    fn zs_contains<K>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> bool
    where
        K: IntoDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we don't use the value at all. This avoids an extra copy.
        self.get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur")
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::{arbitrary::any, prelude::*};

    impl Arbitrary for TransactionLocation {
        type Parameters = ();

        fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
            (any::<block::Height>(), any::<u32>())
                .prop_map(|(height, index)| Self { height, index })
                .boxed()
        }

        type Strategy = BoxedStrategy<Self>;
    }

    fn round_trip<T>(input: T) -> T
    where
        T: IntoDisk + FromDisk,
    {
        let bytes = input.as_bytes();
        T::from_bytes(bytes)
    }

    fn assert_round_trip<T>(input: T)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        let before = input.clone();
        let after = round_trip(input);
        assert_eq!(before, after);
    }

    fn round_trip_ref<T>(input: &T) -> T
    where
        T: IntoDisk + FromDisk,
    {
        let bytes = input.as_bytes();
        T::from_bytes(bytes)
    }

    fn assert_round_trip_ref<T>(input: &T)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        let before = input;
        let after = round_trip_ref(input);
        assert_eq!(before, &after);
    }

    fn round_trip_arc<T>(input: Arc<T>) -> T
    where
        T: IntoDisk + FromDisk,
    {
        let bytes = input.as_bytes();
        T::from_bytes(bytes)
    }

    fn assert_round_trip_arc<T>(input: Arc<T>)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        let before = input.clone();
        let after = round_trip_arc(input);
        assert_eq!(*before, after);
    }

    /// The round trip test covers types that are used as value field in a rocksdb
    /// column family. Only these types are ever deserialized, and so they're the only
    /// ones that implement both `IntoDisk` and `FromDisk`.
    fn assert_value_properties<T>(input: T)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        assert_round_trip_ref(&input);
        assert_round_trip_arc(Arc::new(input.clone()));
        assert_round_trip(input);
    }

    #[test]
    fn roundtrip_transaction_location() {
        zebra_test::init();
        proptest!(|(val in any::<TransactionLocation>())| assert_value_properties(val));
    }

    #[test]
    fn roundtrip_block_hash() {
        zebra_test::init();
        proptest!(|(val in any::<block::Hash>())| assert_value_properties(val));
    }

    #[test]
    fn roundtrip_block_height() {
        zebra_test::init();
        proptest!(|(val in any::<block::Height>())| assert_value_properties(val));
    }

    #[test]
    fn roundtrip_block() {
        zebra_test::init();

        proptest!(|(val in any::<Block>())| assert_value_properties(val));
    }

    #[test]
    fn roundtrip_transparent_output() {
        zebra_test::init();

        proptest!(|(val in any::<transparent::Utxo>())| assert_value_properties(val));
    }
}
