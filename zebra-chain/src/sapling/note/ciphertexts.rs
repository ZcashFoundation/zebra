//! Encrypted parts of Sapling notes.

use std::{fmt, io};

use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to the Sapling 'encCiphertext's
#[derive(Deserialize, Serialize)]
pub struct EncryptedNote(#[serde(with = "BigArray")] pub(crate) [u8; 580]);

impl From<[u8; 580]> for EncryptedNote {
    fn from(byte_array: [u8; 580]) -> Self {
        Self(byte_array)
    }
}

impl fmt::Debug for EncryptedNote {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EncryptedNote")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for EncryptedNote {}

impl Clone for EncryptedNote {
    fn clone(&self) -> Self {
        *self
    }
}

impl PartialEq for EncryptedNote {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for EncryptedNote {}

impl ZcashSerialize for EncryptedNote {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for EncryptedNote {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 580];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

impl From<EncryptedNote> for [u8; 580] {
    fn from(note: EncryptedNote) -> Self {
        note.0
    }
}

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to Sapling's 'outCiphertext'
#[derive(Deserialize, Serialize)]
pub struct WrappedNoteKey(#[serde(with = "BigArray")] pub(crate) [u8; 80]);

impl From<[u8; 80]> for WrappedNoteKey {
    fn from(byte_array: [u8; 80]) -> Self {
        Self(byte_array)
    }
}

impl fmt::Debug for WrappedNoteKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("WrappedNoteKey")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for WrappedNoteKey {}

impl Clone for WrappedNoteKey {
    fn clone(&self) -> Self {
        *self
    }
}

impl PartialEq for WrappedNoteKey {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for WrappedNoteKey {}

impl ZcashSerialize for WrappedNoteKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for WrappedNoteKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 80];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

impl From<WrappedNoteKey> for [u8; 80] {
    fn from(note: WrappedNoteKey) -> Self {
        note.0
    }
}

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
proptest! {

    #[test]
    fn encrypted_ciphertext_roundtrip(ec in any::<EncryptedNote>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        ec.zcash_serialize(&mut data).expect("EncryptedNote should serialize");

        let ec2 = EncryptedNote::zcash_deserialize(&data[..]).expect("randomized EncryptedNote should deserialize");

        prop_assert_eq![ec, ec2];
    }

    #[test]
    fn out_ciphertext_roundtrip(oc in any::<WrappedNoteKey>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        oc.zcash_serialize(&mut data).expect("WrappedNoteKey should serialize");

        let oc2 = WrappedNoteKey::zcash_deserialize(&data[..]).expect("randomized WrappedNoteKey should deserialize");

        prop_assert_eq![oc, oc2];
    }
}
