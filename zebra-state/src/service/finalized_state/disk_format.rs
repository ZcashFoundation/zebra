//! Module defining exactly how to move types in and out of sled
use std::{convert::TryInto, fmt::Debug, sync::Arc};

use zebra_chain::{
    block,
    block::Block,
    sapling,
    serialization::{ZcashDeserialize, ZcashSerialize},
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

    // function to convert the current type into its disk format
    fn into_ivec(&self) -> sled::IVec;
}

impl<'a, T> IntoDisk for &'a T
where
    T: IntoDisk,
{
    type Bytes = T::Bytes;

    fn as_bytes(&self) -> Self::Bytes {
        T::as_bytes(*self)
    }

    fn into_ivec(&self) -> sled::IVec {
        T::into_ivec(*self)
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

    fn into_ivec(&self) -> sled::IVec {
        T::into_ivec(&*self)
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
    fn from_ivec(bytes: sled::IVec) -> Self;
}

impl<T> FromDisk for Arc<T>
where
    T: FromDisk,
{
    fn from_ivec(bytes: sled::IVec) -> Self {
        Arc::new(T::from_ivec(bytes))
    }
}

impl IntoDisk for Block {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().into()
    }
}

impl FromDisk for Block {
    fn from_ivec(bytes: sled::IVec) -> Self {
        Block::zcash_deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoSled")
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

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromDisk for TransactionLocation {
    fn from_ivec(sled_bytes: sled::IVec) -> Self {
        let height = {
            let mut bytes = [0; 4];
            bytes.copy_from_slice(&sled_bytes[0..4]);
            let height = u32::from_be_bytes(bytes);
            block::Height(height)
        };

        let index = {
            let mut bytes = [0; 4];
            bytes.copy_from_slice(&sled_bytes[4..8]);
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

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoDisk for block::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromDisk for block::Hash {
    fn from_ivec(bytes: sled::IVec) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Self(array)
    }
}

impl IntoDisk for sprout::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoDisk for sapling::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoDisk for () {
    type Bytes = [u8; 0];

    fn as_bytes(&self) -> Self::Bytes {
        []
    }

    fn into_ivec(&self) -> sled::IVec {
        sled::IVec::default()
    }
}

impl IntoDisk for block::Height {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromDisk for block::Height {
    fn from_ivec(bytes: sled::IVec) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        block::Height(u32::from_be_bytes(array))
    }
}

impl IntoDisk for transparent::Output {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().into()
    }
}

impl FromDisk for transparent::Output {
    fn from_ivec(bytes: sled::IVec) -> Self {
        Self::zcash_deserialize(&*bytes)
            .expect("deserialization format should match the serialization format used by IntoSled")
    }
}

impl IntoDisk for transparent::OutPoint {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(&self) -> sled::IVec {
        self.as_bytes().into()
    }
}

/// Helper trait for inserting (Key, Value) pairs into sled with a consistently
/// defined format
pub trait DiskSerialize {
    /// Serialize and insert the given key and value into a sled tree.
    fn zs_insert<K, V>(
        &self,
        key: K,
        value: V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: IntoDisk + Debug,
        V: IntoDisk;
}

impl DiskSerialize for sled::transaction::TransactionalTree {
    fn zs_insert<K, V>(
        &self,
        key: K,
        value: V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        use std::any::type_name;

        let key_bytes = key.into_ivec();
        let value_bytes = value.into_ivec();
        let previous = self.insert(key_bytes, value_bytes)?;

        assert!(
            previous.is_none(),
            "duplicate key: previous value for key {:?} was not none when inserting into ({}, {}) sled Tree",
            key,
            type_name::<K>(),
            type_name::<V>()
        );

        Ok(())
    }
}

/// Helper trait for retrieving values from sled trees with a consistently
/// defined format
pub trait DiskDeserialize {
    /// Serialize the given key and use that to get and deserialize the
    /// corresponding value from a sled tree, if it is present.
    fn zs_get<K, V>(&self, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk;
}

impl DiskDeserialize for sled::Tree {
    fn zs_get<K, V>(&self, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk,
    {
        let key_bytes = key.as_bytes();

        let value_bytes = self
            .get(key_bytes)
            .expect("expected that sled errors would not occur");

        value_bytes.map(V::from_ivec)
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
        let bytes = input.into_ivec();
        T::from_ivec(bytes)
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
        let bytes = input.into_ivec();
        T::from_ivec(bytes)
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
        let bytes = input.into_ivec();
        T::from_ivec(bytes)
    }

    fn assert_round_trip_arc<T>(input: Arc<T>)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        let before = input.clone();
        let after = round_trip_arc(input);
        assert_eq!(*before, after);
    }

    /// The round trip test covers types that are used as value field in a sled
    /// Tree. Only these types are ever deserialized, and so they're the only
    /// ones that implement both `IntoSled` and `FromSled`.
    fn assert_value_properties<T>(input: T)
    where
        T: IntoDisk + FromDisk + Clone + PartialEq + std::fmt::Debug,
    {
        assert_round_trip_ref(&input);
        assert_round_trip_arc(Arc::new(input.clone()));
        assert_round_trip(input);
    }

    /// This test asserts that types that are used as sled keys behave correctly.
    /// Any type that implements `IntoIVec` can be used as a sled key. The value
    /// is serialized via `IntoSled::into_ivec` when the `key`, `value` pair is
    /// inserted into the sled tree. The `as_bytes` impl on the other hand is
    /// called for most other operations when comparing a key against existing
    /// keys in the sled database, such as `contains`.
    fn assert_as_bytes_matches_ivec<T>(input: T)
    where
        T: IntoDisk + Clone,
    {
        let before = input.clone();
        let ivec = input.into_ivec();
        assert_eq!(before.as_bytes().as_ref(), ivec.as_ref());
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

        proptest!(|(val in any::<transparent::Output>())| assert_value_properties(val));
    }

    #[test]
    fn key_matches_ivec_transaction_location() {
        zebra_test::init();
        proptest!(|(val in any::<TransactionLocation>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_trans_hash() {
        zebra_test::init();
        proptest!(|(val in any::<transaction::Hash>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_block_hash() {
        zebra_test::init();
        proptest!(|(val in any::<block::Hash>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_sprout_nullifier() {
        zebra_test::init();
        proptest!(|(val in any::<sprout::Nullifier>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_sapling_nullifier() {
        zebra_test::init();
        proptest!(|(val in any::<sapling::Nullifier>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_block_height() {
        zebra_test::init();
        proptest!(|(val in any::<block::Height>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_transparent_output() {
        zebra_test::init();
        proptest!(|(val in any::<transparent::Output>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_transparent_outpoint() {
        zebra_test::init();
        proptest!(|(val in any::<transparent::OutPoint>())| assert_as_bytes_matches_ivec(val));
    }
}
