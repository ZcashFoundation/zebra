use std::{fmt, io};

#[cfg(test)]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::{
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    sha256d_writer::Sha256dWriter,
};

use super::BlockHeader;

/// A SHA-256d hash of a BlockHeader.
///
/// This is useful when one block header is pointing to its parent
/// block header in the block chain. ⛓️
///
/// This is usually called a 'block hash', as it is frequently used
/// to identify the entire block, since the hash preimage includes
/// the merkle root of the transactions in this block. But
/// _technically_, this is just a hash of the block _header_, not
/// the direct bytes of the transactions as well as the header. So
/// for now I want to call it a `BlockHeaderHash` because that's
/// more explicit.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct BlockHeaderHash(pub [u8; 32]);

impl fmt::Debug for BlockHeaderHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("BlockHeaderHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl<'a> From<&'a BlockHeader> for BlockHeaderHash {
    fn from(block_header: &'a BlockHeader) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        block_header
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finish())
    }
}

impl ZcashSerialize for BlockHeaderHash {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)?;
        Ok(())
    }
}

impl ZcashDeserialize for BlockHeaderHash {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(BlockHeaderHash(reader.read_32_bytes()?))
    }
}

impl std::str::FromStr for BlockHeaderHash {
    type Err = SerializationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            Ok(BlockHeaderHash(bytes))
        }
    }
}
