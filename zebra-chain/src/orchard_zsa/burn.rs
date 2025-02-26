//! Orchard ZSA burn related functionality.

use std::io;

use halo2::pasta::pallas;

use crate::{
    amount::Amount,
    block::MAX_BLOCK_BYTES,
    orchard::ValueCommitment,
    serialization::{
        ReadZcashExt, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
};

use orchard::{note::AssetBase, value::NoteValue};

// The size of the serialized AssetBase in bytes (used for TrustedPreallocate impls)
pub(super) const ASSET_BASE_SIZE: u64 = 32;

impl ZcashSerialize for AssetBase {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_bytes())
    }
}

impl ZcashDeserialize for AssetBase {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Option::from(AssetBase::from_bytes(&reader.read_32_bytes()?))
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa AssetBase!"))
    }
}

// The size of the Amount type, in bytes
const AMOUNT_SIZE: u64 = 8;

const BURN_ITEM_SIZE: u64 = ASSET_BASE_SIZE + AMOUNT_SIZE;

/// Represents an OrchardZSA burn item.
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
        (MAX_BLOCK_BYTES - 1) / BURN_ITEM_SIZE
    }
}

impl serde::Serialize for BurnItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (self.0.to_bytes(), &self.1).serialize(serializer)
    }
}

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

/// A special marker type indicating the absence of a burn field in Orchard ShieldedData for `V5` transactions.
/// Useful for unifying ShieldedData serialization and deserialization implementations across various
/// Orchard protocol variants (i.e. various transaction versions).
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoBurn;

impl From<NoBurn> for ValueCommitment {
    fn from(_burn: NoBurn) -> ValueCommitment {
        // FIXME: is there a simpler way to get zero ValueCommitment?
        ValueCommitment::new(pallas::Scalar::zero(), Amount::zero())
    }
}

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

// FIXME: consider conversion from reference to Burn instead, to avoid using `clone` when it's called
impl From<Burn> for ValueCommitment {
    fn from(burn: Burn) -> ValueCommitment {
        burn.0
            .into_iter()
            .map(|BurnItem(asset, amount)| {
                ValueCommitment::with_asset(pallas::Scalar::zero(), amount, &asset)
            })
            .sum()
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
