//! Encrypted parts of Orchard notes.

use std::{fmt, io};

use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to the Orchard 'encCiphertext's
#[derive(Deserialize, Serialize)]
pub struct EncryptedNote(#[serde(with = "BigArray")] pub(crate) [u8; 580]);

// These impls all only exist because of array length restrictions.
// TODO: use const generics https://github.com/ZcashFoundation/zebra/issues/2042

impl Copy for EncryptedNote {}

impl Clone for EncryptedNote {
    fn clone(&self) -> Self {
        *self
    }
}

impl fmt::Debug for EncryptedNote {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EncryptedNote")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

impl Eq for EncryptedNote {}

impl From<[u8; 580]> for EncryptedNote {
    fn from(bytes: [u8; 580]) -> Self {
        EncryptedNote(bytes)
    }
}

impl From<EncryptedNote> for [u8; 580] {
    fn from(enc_ciphertext: EncryptedNote) -> Self {
        enc_ciphertext.0
    }
}
impl From<&EncryptedNote> for [u8; 580] {
    fn from(&EncryptedNote(enc_ciphertext): &EncryptedNote) -> Self {
        enc_ciphertext
    }
}

impl PartialEq for EncryptedNote {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

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

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to Orchard's 'outCiphertext'
#[derive(Deserialize, Serialize)]
pub struct WrappedNoteKey(#[serde(with = "BigArray")] pub(crate) [u8; 80]);

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

impl From<[u8; 80]> for WrappedNoteKey {
    fn from(bytes: [u8; 80]) -> Self {
        WrappedNoteKey(bytes)
    }
}

impl From<WrappedNoteKey> for [u8; 80] {
    fn from(out_ciphertext: WrappedNoteKey) -> Self {
        out_ciphertext.0
    }
}

impl From<&WrappedNoteKey> for [u8; 80] {
    fn from(out_ciphertext: &WrappedNoteKey) -> Self {
        out_ciphertext.0
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
