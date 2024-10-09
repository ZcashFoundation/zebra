//! Orchard ZSA issuance related functionality.

use std::{fmt::Debug, io};

use crate::{
    block::MAX_BLOCK_BYTES,
    serialization::{
        zcash_serialize_empty_list, ReadZcashExt, SerializationError, TrustedPreallocate,
        ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    },
};

use nonempty::NonEmpty;

// FIXME: needed for "as_bool" only - consider to implement as_bool locally
use bitvec::macros::internal::funty::Fundamental;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use orchard::{
    issuance::{IssueAction, IssueBundle, Signed},
    keys::IssuanceValidatingKey,
    note::{RandomSeed, Rho},
    primitives::redpallas::{SigType, Signature, SpendAuth},
    value::NoteValue,
    Address, Note,
};

use super::common::ASSET_BASE_SIZE;

/// Wrapper for `IssueBundle` used in the context of Transaction V6. This allows the implementation of
/// a Serde serializer for unit tests within this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueData(IssueBundle<Signed>);

impl From<IssueBundle<Signed>> for IssueData {
    fn from(inner: IssueBundle<Signed>) -> Self {
        Self(inner)
    }
}

// Sizes of the serialized values for types in bytes (used for TrustedPreallocate impls)
// FIXME: are those values correct (43, 32 etc.)?
//const ISSUANCE_VALIDATING_KEY_SIZE: u64 = 32;
const ADDRESS_SIZE: u64 = 43;
const NULLIFIER_SIZE: u64 = 32;
const NOTE_VALUE_SIZE: u64 = 4;
const RANDOM_SEED_SIZE: u64 = 32;
// FIXME: is this a correct way to calculate (simple sum of sizes of components)?
const NOTE_SIZE: u64 =
    ADDRESS_SIZE + NOTE_VALUE_SIZE + ASSET_BASE_SIZE + NULLIFIER_SIZE + RANDOM_SEED_SIZE;

// FIXME: duplicates ZcashSerialize for reddsa::Signature in transaction/serialize.rs
// (as Signature from oechard_zsa is formally a different type)
impl<T: SigType> ZcashSerialize for Signature<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 64]>::from(self))?;
        Ok(())
    }
}

// FIXME: duplicates ZcashDeserialize for reddsa::Signature in transaction/serialize.rs
// (as Signature from oechard_zsa is formally a different type)
impl<T: SigType> ZcashDeserialize for Signature<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_64_bytes()?.into())
    }
}

impl ZcashDeserialize for Signed {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let signature = Signature::<SpendAuth>::zcash_deserialize(&mut reader)?;
        Ok(Signed::from_data((&signature).into()))
    }
}

impl ZcashSerialize for IssuanceValidatingKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_bytes())
    }
}

impl ZcashDeserialize for IssuanceValidatingKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        IssuanceValidatingKey::from_bytes(&reader.read_32_bytes()?)
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa IssuanceValidatingKey!"))
    }
}

impl ZcashSerialize for Address {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_raw_address_bytes())
    }
}

impl ZcashDeserialize for Address {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0u8; ADDRESS_SIZE as usize];
        reader.read_exact(&mut bytes)?;
        Option::from(Address::from_raw_address_bytes(&bytes))
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa Address!"))
    }
}

impl ZcashSerialize for Rho {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.to_bytes())
    }
}

impl ZcashDeserialize for Rho {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Option::from(Rho::from_bytes(&reader.read_32_bytes()?))
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa Rho!"))
    }
}

impl ZcashSerialize for RandomSeed {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(self.as_bytes())
    }
}

// RandomSeed::zcash_deserialize can't be implemented and used as it requires Nullifier parameter.
// That's why we need to have this function.
fn zcash_deserialize_random_seed<R: io::Read>(
    mut reader: R,
    rho: &Rho,
) -> Result<RandomSeed, SerializationError> {
    Option::from(RandomSeed::from_bytes(reader.read_32_bytes()?, rho))
        .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa RandomSeed!"))
}

impl ZcashSerialize for NoteValue {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // FIXME: use Amount serializer/deserializer?
        writer.write_u64::<LittleEndian>(self.inner())?;
        Ok(())
    }
}

impl ZcashDeserialize for NoteValue {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(NoteValue::from_raw(reader.read_u64::<LittleEndian>()?))
    }
}

impl ZcashSerialize for Note {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.recipient().zcash_serialize(&mut writer)?;
        self.value().zcash_serialize(&mut writer)?;
        self.asset().zcash_serialize(&mut writer)?;
        self.rho().zcash_serialize(&mut writer)?;
        self.rseed().zcash_serialize(&mut writer)?;

        Ok(())
    }
}

impl ZcashDeserialize for Note {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let recipient = (&mut reader).zcash_deserialize_into()?;
        let value = (&mut reader).zcash_deserialize_into()?;
        let asset = (&mut reader).zcash_deserialize_into()?;
        let rho = (&mut reader).zcash_deserialize_into()?;
        let rseed = zcash_deserialize_random_seed(&mut reader, &rho)?;

        Option::from(Note::from_parts(recipient, value, asset, rho, rseed))
            .ok_or_else(|| SerializationError::Parse("Invalid orchard_zsa Note components!"))
    }
}

impl TrustedPreallocate for Note {
    fn max_allocation() -> u64 {
        // FIXME: is this a correct calculation way?
        // The longest Vec<Note> we receive from an honest peer must fit inside a valid block.
        // Since encoding the length of the vec takes at least one byte, we use MAX_BLOCK_BYTES - 1
        (MAX_BLOCK_BYTES - 1) / NOTE_SIZE
    }
}

impl ZcashSerialize for IssueAction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u8(self.is_finalized().as_u8())?;
        self.notes().zcash_serialize(&mut writer)?;
        self.asset_desc().zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for IssueAction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let finalize = reader.read_u8()?.as_bool();
        let notes = (&mut reader).zcash_deserialize_into()?;
        let asset_descr = (&mut reader).zcash_deserialize_into()?;
        Ok(IssueAction::from_parts(asset_descr, notes, finalize))
    }
}

impl TrustedPreallocate for IssueAction {
    fn max_allocation() -> u64 {
        // FIXME: impl correct calculation
        10
    }
}

impl ZcashSerialize for IssueBundle<Signed> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // FIXME: try to avoid transforming to Vec (consider implementation of ZcashSerialize for IntoIter generic,
        // or use AtLeastOne).
        // This is how does it work in librustzcash:
        // Vector::write_nonempty(&mut writer, bundle.actions(), |w, action| write_action(action, w))?;
        let actions: Vec<_> = self.actions().clone().into();

        actions.zcash_serialize(&mut writer)?;
        self.ik().zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.authorization().signature()))?;
        Ok(())
    }
}

impl ZcashSerialize for Option<IssueData> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            None => {
                // Denoted as `&Option<IssueData>` in the spec (ZIP 230).
                zcash_serialize_empty_list(writer)?;
            }
            Some(issue_data) => {
                issue_data.0.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

// FIXME: We can't split IssueData out of Option<IssueData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<IssueData> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let actions: Vec<_> = (&mut reader).zcash_deserialize_into()?;

        if actions.is_empty() {
            Ok(None)
        } else {
            let ik = (&mut reader).zcash_deserialize_into()?;
            let authorization = (&mut reader).zcash_deserialize_into()?;

            Ok(Some(IssueData(IssueBundle::from_parts(
                ik,
                NonEmpty::from_vec(actions).ok_or_else(|| {
                    SerializationError::Parse("Invalid orchard_zsa IssueData - no actions!")
                })?,
                authorization,
            ))))
        }
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl serde::Serialize for IssueData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // TODO: FIXME: implement Serde serialization here
        "(IssueData)".serialize(serializer)
    }
}
