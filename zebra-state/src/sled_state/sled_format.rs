//! Module defining exactly how to move types in and out of sled
use std::{convert::TryInto, sync::Arc};

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

// Helper trait for defining the exact format used to interact with sled per
// type.
pub trait IntoSled {
    // The type used to compare a value as a key to other keys stored in a
    // sled::Tree
    type Bytes: AsRef<[u8]>;

    // function to convert the current type to its sled format in `zs_get()`
    // without necessarily allocating a new IVec
    fn as_bytes(&self) -> Self::Bytes;

    // function to convert the current type into its sled format
    fn into_ivec(self) -> sled::IVec;
}

/// Helper type for retrieving types from sled with the correct format.
///
/// The ivec should be correctly encoded by IntoSled.
pub trait FromSled: Sized {
    /// Function to convert the sled bytes back into the deserialized type.
    ///
    /// # Panics
    ///
    /// - if the input data doesn't deserialize correctly
    fn from_ivec(bytes: sled::IVec) -> Self;
}

impl IntoSled for &Block {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().into()
    }
}

impl FromSled for Arc<Block> {
    fn from_ivec(bytes: sled::IVec) -> Self {
        Arc::<Block>::zcash_deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoSled")
    }
}

impl IntoSled for TransactionLocation {
    type Bytes = [u8; 8];

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.0.to_be_bytes();
        let index_bytes = self.index.to_be_bytes();

        let mut bytes = [0; 8];

        bytes[0..4].copy_from_slice(&height_bytes);
        bytes[4..8].copy_from_slice(&index_bytes);

        bytes
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromSled for TransactionLocation {
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

impl IntoSled for transaction::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoSled for block::Hash {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }
    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromSled for block::Hash {
    fn from_ivec(bytes: sled::IVec) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Self(array)
    }
}

impl IntoSled for &sprout::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoSled for &sapling::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.0
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl IntoSled for () {
    type Bytes = [u8; 0];

    fn as_bytes(&self) -> Self::Bytes {
        []
    }

    fn into_ivec(self) -> sled::IVec {
        sled::IVec::default()
    }
}

impl IntoSled for block::Height {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().as_ref().into()
    }
}

impl FromSled for block::Height {
    fn from_ivec(bytes: sled::IVec) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        block::Height(u32::from_be_bytes(array))
    }
}

impl IntoSled for &transparent::Output {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().into()
    }
}

impl FromSled for transparent::Output {
    fn from_ivec(bytes: sled::IVec) -> Self {
        Self::zcash_deserialize(&*bytes)
            .expect("deserialization format should match the serialization format used by IntoSled")
    }
}

impl IntoSled for transparent::OutPoint {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().into()
    }
}

/// Helper trait for inserting (Key, Value) pairs into sled with a consistently
/// defined format
pub trait SledSerialize {
    /// Serialize and insert the given key and value into a sled tree.
    fn zs_insert<K, V>(
        &self,
        key: K,
        value: V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: IntoSled,
        V: IntoSled;
}

impl SledSerialize for sled::transaction::TransactionalTree {
    fn zs_insert<K, V>(
        &self,
        key: K,
        value: V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: IntoSled,
        V: IntoSled,
    {
        let key_bytes = key.into_ivec();
        let value_bytes = value.into_ivec();
        self.insert(key_bytes, value_bytes)?;
        Ok(())
    }
}

/// Helper trait for retrieving values from sled trees with a consistently
/// defined format
pub trait SledDeserialize {
    /// Serialize the given key and use that to get and deserialize the
    /// corresponding value from a sled tree, if it is present.
    fn zs_get<K, V>(&self, key: &K) -> Option<V>
    where
        K: IntoSled,
        V: FromSled;
}

impl SledDeserialize for sled::Tree {
    fn zs_get<K, V>(&self, key: &K) -> Option<V>
    where
        K: IntoSled,
        V: FromSled,
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
    use std::ops::Deref;

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
        T: IntoSled + FromSled,
    {
        let bytes = input.into_ivec();
        T::from_ivec(bytes)
    }

    /// The round trip test covers types that are used as value field in a sled
    /// Tree. Only these types are ever deserialized, and so they're the only
    /// ones that implement both `IntoSled` and `FromSled`.
    fn assert_round_trip<T>(input: T)
    where
        T: IntoSled + FromSled + Clone + PartialEq + std::fmt::Debug,
    {
        let before = input.clone();
        let after = round_trip(input);
        assert_eq!(before, after);
    }

    /// This test asserts that types that are used as sled keys behave correctly.
    /// Any type that implements `IntoIVec` can be used as a sled key. The value
    /// is serialized via `IntoSled::into_ivec` when the `key`, `value` pair is
    /// inserted into the sled tree. The `as_bytes` impl on the other hand is
    /// called for most other operations when comparing a key against existing
    /// keys in the sled database, such as `contains`.
    fn assert_as_bytes_matches_ivec<T>(input: T)
    where
        T: IntoSled + Clone,
    {
        let before = input.clone();
        let ivec = input.into_ivec();
        assert_eq!(before.as_bytes().as_ref(), ivec.as_ref());
    }

    #[test]
    fn roundtrip_transaction_location() {
        zebra_test::init();
        proptest!(|(val in any::<TransactionLocation>())| assert_round_trip(val));
    }

    #[test]
    fn roundtrip_block_hash() {
        zebra_test::init();
        proptest!(|(val in any::<block::Hash>())| assert_round_trip(val));
    }

    #[test]
    fn roundtrip_block_height() {
        zebra_test::init();
        proptest!(|(val in any::<block::Height>())| assert_round_trip(val));
    }

    #[test]
    fn roundtrip_block() {
        zebra_test::init();

        proptest!(|(block in any::<Block>())| {
            let bytes = block.into_ivec();
            let deserialized: Arc<Block> = FromSled::from_ivec(bytes);
            assert_eq!(&block, deserialized.deref());
        });
    }

    #[test]
    fn roundtrip_transparent_output() {
        zebra_test::init();

        proptest!(|(block in any::<transparent::Output>())| {
            let bytes = block.into_ivec();
            let deserialized: transparent::Output = FromSled::from_ivec(bytes);
            assert_eq!(block, deserialized);
        });
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
        proptest!(|(val in any::<sprout::Nullifier>())| assert_as_bytes_matches_ivec(&val));
    }

    #[test]
    fn key_matches_ivec_sapling_nullifier() {
        zebra_test::init();
        proptest!(|(val in any::<sapling::Nullifier>())| assert_as_bytes_matches_ivec(&val));
    }

    #[test]
    fn key_matches_ivec_block_height() {
        zebra_test::init();
        proptest!(|(val in any::<block::Height>())| assert_as_bytes_matches_ivec(val));
    }

    #[test]
    fn key_matches_ivec_transparent_output() {
        zebra_test::init();
        proptest!(|(val in any::<transparent::Output>())| assert_as_bytes_matches_ivec(&val));
    }

    #[test]
    fn key_matches_ivec_transparent_outpoint() {
        zebra_test::init();
        proptest!(|(val in any::<transparent::OutPoint>())| assert_as_bytes_matches_ivec(val));
    }
}
