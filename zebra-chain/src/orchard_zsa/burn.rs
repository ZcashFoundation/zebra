//! Orchard ZSA burn related functionality.

use std::io;

use crate::{
    amount::Amount,
    block::MAX_BLOCK_BYTES,
    serialization::{SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize},
};

use orchard::note::AssetBase;

use super::serialize::ASSET_BASE_SIZE;

// The size of the Amount type, in bytes
const AMOUNT_SIZE: u64 = 8;

const BURN_ITEM_SIZE: u64 = ASSET_BASE_SIZE + AMOUNT_SIZE;

/// Represents an OrchardZSA burn item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnItem(AssetBase, Amount);

impl ZcashSerialize for BurnItem {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let BurnItem(asset_base, amount) = self;

        asset_base.zcash_serialize(&mut writer)?;
        amount.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

impl ZcashDeserialize for BurnItem {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            AssetBase::zcash_deserialize(&mut reader)?,
            Amount::zcash_deserialize(&mut reader)?,
        ))
    }
}

impl TrustedPreallocate for BurnItem {
    fn max_allocation() -> u64 {
        (MAX_BLOCK_BYTES - 1) / BURN_ITEM_SIZE
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl serde::Serialize for BurnItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (self.0.to_bytes(), &self.1).serialize(serializer)
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl<'de> serde::Deserialize<'de> for BurnItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (asset_base_bytes, amount) = <([u8; 32], Amount)>::deserialize(deserializer)?;
        Ok(BurnItem(
            Option::from(AssetBase::from_bytes(&asset_base_bytes))
                .ok_or_else(|| serde::de::Error::custom("Invalid orchard_zsa AssetBase"))?,
            amount,
        ))
    }
}
