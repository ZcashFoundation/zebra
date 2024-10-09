//! Orchard ZSA burn related functionality.

use std::io;

use crate::{
    amount::Amount,
    block::MAX_BLOCK_BYTES,
    serialization::{SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize},
};

use orchard::{note::AssetBase, value::NoteValue};

use super::common::ASSET_BASE_SIZE;

// Sizes of the serialized values for types in bytes (used for TrustedPreallocate impls)
const AMOUNT_SIZE: u64 = 8;

// FIXME: is this a correct way to calculate (simple sum of sizes of components)?
const BURN_ITEM_SIZE: u64 = ASSET_BASE_SIZE + AMOUNT_SIZE;

/// Orchard ZSA burn item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnItem(AssetBase, Amount);

// Convert from burn item type used in `orchard` crate
impl TryFrom<(AssetBase, NoteValue)> for BurnItem {
    type Error = crate::amount::Error;

    fn try_from(item: (AssetBase, NoteValue)) -> Result<Self, Self::Error> {
        Ok(Self(item.0, item.1.inner().try_into()?))
    }
}

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
        // FIXME: is this a correct calculation way?
        // The longest Vec<BurnItem> we receive from an honest peer must fit inside a valid block.
        // Since encoding the length of the vec takes at least one byte, we use MAX_BLOCK_BYTES - 1
        (MAX_BLOCK_BYTES - 1) / BURN_ITEM_SIZE
    }
}

impl serde::Serialize for BurnItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // FIXME: return a custom error with a meaningful description?
        (self.0.to_bytes(), &self.1).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for BurnItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // FIXME: consider another implementation (explicit specifying of [u8; 32] may not look perfect)
        let (asset_base_bytes, amount) = <([u8; 32], Amount)>::deserialize(deserializer)?;
        // FIXME: return custom error with a meaningful description?
        Ok(BurnItem(
            // FIXME: duplicates the body of AssetBase::zcash_deserialize?
            Option::from(AssetBase::from_bytes(&asset_base_bytes))
                .ok_or_else(|| serde::de::Error::custom("Invalid orchard_zsa AssetBase"))?,
            amount,
        ))
    }
}

/// A special marker type indicating the absence of a burn field in Orchard ShieldedData for `V5` transactions.
/// Useful for unifying ShieldedData serialization and deserialization implementations across various
/// Orchard protocol variants (i.e. various transaction versions).
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoBurn;

impl ZcashSerialize for NoBurn {
    fn zcash_serialize<W: io::Write>(&self, mut _writer: W) -> Result<(), io::Error> {
        Ok(())
    }
}

impl ZcashDeserialize for NoBurn {
    fn zcash_deserialize<R: io::Read>(mut _reader: R) -> Result<Self, SerializationError> {
        Ok(Self)
    }
}

/// Orchard ZSA burn items (assets intended for burning)
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Burn(Vec<BurnItem>);

impl From<Vec<BurnItem>> for Burn {
    fn from(inner: Vec<BurnItem>) -> Self {
        Self(inner)
    }
}

impl ZcashSerialize for Burn {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        self.0.zcash_serialize(writer)
    }
}

impl ZcashDeserialize for Burn {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(Burn(Vec::<BurnItem>::zcash_deserialize(reader)?))
    }
}
