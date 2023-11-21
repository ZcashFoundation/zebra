//! Orchard ZSA burn related functionality.

use std::io;

use crate::serialization::{
    SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
};

use crate::amount::Amount;

use orchard_zsa::note::AssetBase;

/// Represents an Orchard ZSA burn item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnItem(AssetBase, Amount);

impl ZcashSerialize for BurnItem {
    fn zcash_serialize<W: io::Write>(&self, mut _writer: W) -> Result<(), io::Error> {
        // TODO: FIXME: implement serialization
        Ok(())
    }
}

impl ZcashDeserialize for BurnItem {
    fn zcash_deserialize<R: io::Read>(mut _reader: R) -> Result<Self, SerializationError> {
        // TODO: FIXME: implement deserialization
        Ok(Self(AssetBase::native(), Amount::zero()))
    }
}

impl TrustedPreallocate for BurnItem {
    fn max_allocation() -> u64 {
        // TODO: FIXME: is this a correct calculation way?
        AssetBase::max_allocation() + Amount::max_allocation()
    }
}

impl serde::Serialize for BurnItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // TODO: FIXME: implement serialization
        ().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for BurnItem {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // TODO: FIXME: implement deserialization
        Ok(Self(AssetBase::native(), Amount::zero()))
    }
}
