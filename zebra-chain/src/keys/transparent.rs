use std::io;

use secp256k1::PublicKey;

use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

impl ZcashSerialize for PublicKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.serialize())?;
        Ok(())
    }
}

impl ZcashDeserialize for PublicKey {
    fn zcash_deserialize<R: io::Read>(mut _reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
