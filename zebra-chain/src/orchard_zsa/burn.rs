//! Orchard ZSA burn related functionality.

use std::io;

use halo2::pasta::pallas;

use orchard::{note::AssetBase, value::NoteValue};

use zcash_primitives::transaction::components::orchard::{read_burn, write_burn};

use crate::{
    amount::Amount,
    orchard::ValueCommitment,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

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

// FIXME: use Amount insstead of Amount, remove both TryFrom<...> after that
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

impl TryFrom<BurnItem> for (AssetBase, NoteValue) {
    type Error = std::io::Error;

    fn try_from(item: BurnItem) -> Result<Self, Self::Error> {
        Ok((
            item.0,
            NoteValue::from_raw(
                i64::from(item.1)
                    .try_into()
                    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?,
            ),
        ))
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

impl AsRef<[BurnItem]> for NoBurn {
    fn as_ref(&self) -> &[BurnItem] {
        &[]
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

impl AsRef<[BurnItem]> for Burn {
    fn as_ref(&self) -> &[BurnItem] {
        &self.0
    }
}

impl ZcashSerialize for Burn {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        write_burn(
            &mut writer,
            &self
                .0
                .iter()
                .map(|item| item.clone().try_into())
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl ZcashDeserialize for Burn {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Burn(
            read_burn(&mut reader)?
                .into_iter()
                .map(|item| item.try_into())
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
}
