//! Encrypted parts of Orchard notes.

use std::{fmt, io};

use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to the Orchard 'encCiphertext's
#[derive(Deserialize, Serialize, Clone, Debug, Eq, PartialEq)]
pub struct EncryptedNote<const SIZE: usize>(#[serde(with = "BigArray")] pub(crate) [u8; SIZE]);

impl<const SIZE: usize> From<[u8; SIZE]> for EncryptedNote<SIZE> {
    fn from(bytes: [u8; SIZE]) -> Self {
        Self(bytes)
    }
}

impl<const SIZE: usize> From<EncryptedNote<SIZE>> for [u8; SIZE] {
    fn from(enc_ciphertext: EncryptedNote<SIZE>) -> Self {
        enc_ciphertext.0
    }
}

impl<const SIZE: usize> TryFrom<&[u8]> for EncryptedNote<SIZE> {
    type Error = std::array::TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self(bytes.try_into()?))
    }
}

impl<const SIZE: usize> ZcashSerialize for EncryptedNote<SIZE> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl<const SIZE: usize> ZcashDeserialize for EncryptedNote<SIZE> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; SIZE];
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
mod tests {
    use crate::{
        orchard::{OrchardVanilla, ShieldedDataFlavor, WrappedNoteKey},
        serialization::{ZcashDeserialize, ZcashSerialize},
    };

    #[cfg(feature = "tx_v6")]
    use crate::orchard::OrchardZSA;

    use proptest::prelude::*;

    fn roundtrip_encrypted_note<EncryptedNote>(note: &EncryptedNote) -> EncryptedNote
    where
        EncryptedNote: ZcashSerialize + ZcashDeserialize,
    {
        let mut data = Vec::new();
        note.zcash_serialize(&mut data)
            .expect("EncryptedNote should serialize");
        EncryptedNote::zcash_deserialize(&data[..])
            .expect("randomized EncryptedNote should deserialize")
    }

    proptest! {
        #[test]
        fn encrypted_ciphertext_roundtrip_orchard_vanilla(ec in any::<<OrchardVanilla as ShieldedDataFlavor>::EncryptedNote>()) {
            let _init_guard = zebra_test::init();
            let ec2 = roundtrip_encrypted_note(&ec);
            prop_assert_eq![ec, ec2];
        }


        #[cfg(feature = "tx_v6")]
        #[test]
        fn encrypted_ciphertext_roundtrip_orchard_zsa(ec in any::<<OrchardZSA as ShieldedDataFlavor>::EncryptedNote>()) {
            let _init_guard = zebra_test::init();
            let ec2 = roundtrip_encrypted_note(&ec);
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
}
