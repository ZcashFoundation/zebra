use std::{fmt, io};

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A ciphertext component for encrypted output notes.
///
/// Corresponds to the Sprout 'encCiphertext's
#[derive(Serialize, Deserialize)]
pub struct EncryptedNote(#[serde(with = "BigArray")] pub [u8; 601]);

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
        let mut bytes = [0; 601];
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
        let mut bytes = [0; 601];
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
        zebra_test::init();

        let mut data = Vec::new();

        ec.zcash_serialize(&mut data).expect("EncryptedNote should serialize");

        let ec2 = EncryptedNote::zcash_deserialize(&data[..]).expect("randomized EncryptedNote should deserialize");

        prop_assert_eq![ec, ec2];
    }
}
