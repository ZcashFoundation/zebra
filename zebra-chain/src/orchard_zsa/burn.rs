//! OrchardZSA burn related functionality.

use std::io;

use halo2::pasta::pallas;

use orchard::{note::AssetBase, value::NoteValue};

use zcash_primitives::transaction::components::orchard::{read_burn, write_burn};

use crate::{
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

/// OrchardZSA burn item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BurnItem(AssetBase, NoteValue);

impl BurnItem {
    /// Returns [`AssetBase`] being burned.
    pub fn asset(&self) -> AssetBase {
        self.0
    }

    /// Returns the amount being burned.
    pub fn amount(&self) -> NoteValue {
        self.1
    }

    /// Returns the raw [`u64`] amount being burned.
    pub fn raw_amount(&self) -> u64 {
        self.1.inner()
    }
}

// Convert from burn item type used in `orchard` crate
impl From<(AssetBase, NoteValue)> for BurnItem {
    fn from(item: (AssetBase, NoteValue)) -> Self {
        Self(item.0, item.1)
    }
}

// Convert to burn item type used in `orchard` crate
impl From<BurnItem> for (AssetBase, NoteValue) {
    fn from(item: BurnItem) -> Self {
        (item.0, item.1)
    }
}

impl serde::Serialize for BurnItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (self.0.to_bytes(), &self.1.inner()).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for BurnItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (asset_base_bytes, amount) = <([u8; 32], u64)>::deserialize(deserializer)?;
        Ok(BurnItem(
            Option::from(AssetBase::from_bytes(&asset_base_bytes))
                .ok_or_else(|| serde::de::Error::custom("Invalid orchard_zsa AssetBase"))?,
            NoteValue::from_raw(amount),
        ))
    }
}

/// A special marker type indicating the absence of a burn field in Orchard ShieldedData for `V5`
/// transactions. It is unifying handling and serialization of ShieldedData across various Orchard
/// protocol variants.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoBurn;

impl From<&[(AssetBase, NoteValue)]> for NoBurn {
    fn from(bundle_burn: &[(AssetBase, NoteValue)]) -> Self {
        assert!(
            bundle_burn.is_empty(),
            "Burn must be empty for OrchardVanilla"
        );
        Self
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

/// OrchardZSA burn items.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Burn(Vec<BurnItem>);

impl From<Vec<BurnItem>> for Burn {
    fn from(inner: Vec<BurnItem>) -> Self {
        Self(inner)
    }
}

impl From<&[(AssetBase, NoteValue)]> for Burn {
    fn from(bundle_burn: &[(AssetBase, NoteValue)]) -> Self {
        Self(
            bundle_burn
                .iter()
                .map(|bundle_burn_item| BurnItem::from(*bundle_burn_item))
                .collect(),
        )
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
            &self.0.iter().map(|item| (*item).into()).collect::<Vec<_>>(),
        )
    }
}

impl ZcashDeserialize for Burn {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Burn(
            read_burn(&mut reader)?
                .into_iter()
                .map(|item| item.into())
                .collect(),
        ))
    }
}

/// Computes the value commitment for a list of burns.
///
/// For burns, the public trapdoor is always zero.
pub(crate) fn compute_burn_value_commitment(burn: &[BurnItem]) -> ValueCommitment {
    burn.iter()
        .map(|&BurnItem(asset, amount)| {
            ValueCommitment::new(pallas::Scalar::zero(), amount.into(), asset)
        })
        .sum()
}
