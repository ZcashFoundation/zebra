use std::{fmt, io};

#[cfg(test)]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::serialization::{
    sha256d, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

use super::Header;

/// A hash of a block, used to identify blocks and link blocks into a chain. ⛓️
///
/// Technically, this is the (SHA256d) hash of a block *header*, but since the
/// block header includes the Merkle root of the transaction Merkle tree, it
/// binds the entire contents of the block and is used to identify entire blocks.
///
/// Note: Zebra displays transaction and block hashes in their actual byte-order,
/// not in reversed byte-order.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Hash(pub [u8; 32]);

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&hex::encode(&self.0))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("block::Hash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl<'a> From<&'a Header> for Hash {
    fn from(block_header: &'a Header) -> Self {
        let mut hash_writer = sha256d::Writer::default();
        block_header
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finish())
    }
}

impl ZcashSerialize for Hash {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)?;
        Ok(())
    }
}

impl ZcashDeserialize for Hash {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Hash(reader.read_32_bytes()?))
    }
}

impl std::str::FromStr for Hash {
    type Err = SerializationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            Ok(Hash(bytes))
        }
    }
}
