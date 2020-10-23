//! Module defining exactly how to move types in and out of sled
use std::{convert::TryInto, sync::Arc};

use zebra_chain::{
    block,
    block::Block,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction,
    transaction::Transaction,
    transparent,
};

use crate::BoxError;

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

// Helper type for retrieving types from sled with the correct format
pub trait FromSled: Sized {
    // function to convert the sled bytes back into the deserialized type
    fn from_ivec(bytes: sled::IVec) -> Result<Self, BoxError>;
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
    fn from_ivec(bytes: sled::IVec) -> Result<Self, BoxError> {
        let block = Arc::<Block>::zcash_deserialize(bytes.as_ref())?;
        Ok(block)
    }
}

impl IntoSled for &Arc<Transaction> {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail")
    }

    fn into_ivec(self) -> sled::IVec {
        self.as_bytes().into()
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
    fn from_ivec(bytes: sled::IVec) -> Result<Self, BoxError> {
        let array = bytes.as_ref().try_into().unwrap();
        Ok(Self(array))
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
    fn from_ivec(bytes: sled::IVec) -> Result<Self, BoxError> {
        let array = bytes.as_ref().try_into().unwrap();
        Ok(block::Height(u32::from_be_bytes(array)))
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
    fn from_ivec(bytes: sled::IVec) -> Result<Self, BoxError> {
        Self::zcash_deserialize(&*bytes).map_err(Into::into)
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
    fn zs_get<K, V>(&self, key: &K) -> Result<Option<V>, BoxError>
    where
        K: IntoSled,
        V: FromSled;
}

impl SledDeserialize for sled::Tree {
    fn zs_get<K, V>(&self, key: &K) -> Result<Option<V>, BoxError>
    where
        K: IntoSled,
        V: FromSled,
    {
        let key_bytes = key.as_bytes();

        let value_bytes = self.get(key_bytes)?;

        let value = value_bytes.map(V::from_ivec).transpose()?;

        Ok(value)
    }
}
