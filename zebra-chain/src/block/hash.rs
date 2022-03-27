use std::{fmt, io};

use hex::{FromHex, ToHex};

#[cfg(any(test, feature = "proptest-impl"))]
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
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays transaction and block hashes in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        reversed_bytes
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("block::Hash")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl ToHex for &Hash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for Hash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for Hash {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
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
            bytes.reverse();
            Ok(Hash(bytes))
        }
    }
}
