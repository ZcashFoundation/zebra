use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::any, prelude::*};

use crate::serialization::{serde_helpers, SerializationError, ZcashDeserialize, ZcashSerialize};

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to the Sapling 'encCiphertext's
#[derive(Deserialize, Serialize)]
pub struct EncryptedNote(#[serde(with = "serde_helpers::BigArray")] pub [u8; 580]);

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
        let mut bytes = [0; 580];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
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

/// A ciphertext component for encrypted output notes.
#[derive(Deserialize, Serialize)]
pub struct OutCiphertext(#[serde(with = "serde_helpers::BigArray")] pub [u8; 80]);

impl fmt::Debug for OutCiphertext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutCiphertext")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for OutCiphertext {}

impl Clone for OutCiphertext {
    fn clone(&self) -> Self {
        let mut bytes = [0; 80];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl PartialEq for OutCiphertext {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for OutCiphertext {}

impl ZcashSerialize for OutCiphertext {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for OutCiphertext {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 80];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn encrypted_ciphertext_roundtrip(ec in any::<EncryptedNote>()) {

        let mut data = Vec::new();

        ec.zcash_serialize(&mut data).expect("EncryptedNote should serialize");

        let ec2 = EncryptedNote::zcash_deserialize(&data[..]).expect("randomized EncryptedNote should deserialize");

        prop_assert_eq![ec, ec2];
    }

    #[test]
    fn out_ciphertext_roundtrip(oc in any::<OutCiphertext>()) {

        let mut data = Vec::new();

        oc.zcash_serialize(&mut data).expect("OutCiphertext should serialize");

        let oc2 = OutCiphertext::zcash_deserialize(&data[..]).expect("randomized OutCiphertext should deserialize");

        prop_assert_eq![oc, oc2];
    }
}
