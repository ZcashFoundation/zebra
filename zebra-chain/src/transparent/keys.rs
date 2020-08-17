//! Transparent key trait impls, around secp256k1::PublicKey
//!
//! We don't impl Arbitrary for PublicKey since it's being pulled in
//! from secp256k1 and we don't want to wrap it.

use std::io;

use secp256k1::PublicKey;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

impl ZcashSerialize for PublicKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.serialize())?;
        Ok(())
    }
}

impl ZcashDeserialize for PublicKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 33];
        reader.read_exact(&mut bytes[..])?;
        Self::from_slice(&bytes[..])
            .map_err(|_| SerializationError::Parse("invalid secp256k1 compressed public key"))
    }
}
