use std::{fmt, io, sync::Arc};

use hex::{FromHex, ToHex};
use serde::{Deserialize, Serialize};

use crate::serialization::{
    sha256d, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize, BytesInDisplayOrder,
};

use super::Header;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A hash of a block, used to identify blocks and link blocks into a chain. ⛓️
///
/// Technically, this is the (SHA256d) hash of a block *header*, but since the
/// block header includes the Merkle root of the transaction Merkle tree, it
/// binds the entire contents of the block and is used to identify entire blocks.
///
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary, Default))]
pub struct Hash(pub [u8; 32]);

impl BytesInDisplayOrder for Hash {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        Hash(bytes)
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
        let hash = <[u8; 32]>::from_hex(hex)?;

        Ok(Self::from_bytes_in_display_order(&hash))
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

impl From<Header> for Hash {
    // The borrow is actually needed to use From<&Header>
    #[allow(clippy::needless_borrow)]
    fn from(block_header: Header) -> Self {
        (&block_header).into()
    }
}

impl From<&Arc<Header>> for Hash {
    // The borrow is actually needed to use From<&Header>
    #[allow(clippy::needless_borrow)]
    fn from(block_header: &Arc<Header>) -> Self {
        block_header.as_ref().into()
    }
}

impl From<Arc<Header>> for Hash {
    // The borrow is actually needed to use From<&Header>
    #[allow(clippy::needless_borrow)]
    fn from(block_header: Arc<Header>) -> Self {
        block_header.as_ref().into()
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
        Ok(Self::from_hex(s)?)
    }
}
