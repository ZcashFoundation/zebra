//! Newtype wrappers for primitive data types with semantic meaning.

use std::{fmt, io};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use hex;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A 4-byte checksum using truncated double-SHA256 (two rounds of SHA256).
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Sha256dChecksum(pub [u8; 4]);

impl<'a> From<&'a [u8]> for Sha256dChecksum {
    fn from(bytes: &'a [u8]) -> Self {
        use sha2::{Digest, Sha256};
        let hash1 = Sha256::digest(bytes);
        let hash2 = Sha256::digest(&hash1);
        let mut checksum = [0u8; 4];
        checksum[0..4].copy_from_slice(&hash2[0..4]);
        Self(checksum)
    }
}

impl fmt::Debug for Sha256dChecksum {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Sha256dChecksum")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

/// A u32 which represents a block height value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockHeight(pub u32);

/// A Bitcoin-style `locktime`, representing either a block height or an epoch
/// time.
///
/// # Invariants
///
/// Users should not construct a `LockTime` with `BlockHeight` greater than or
/// equal to `500_000_000` or a timestamp before 4 November 1985 (Unix timestamp
/// less than `500_000_000`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LockTime {
    /// Unlock at a particular block height.
    Height(BlockHeight),
    /// Unlock at a particular time.
    Time(DateTime<Utc>),
}

impl ZcashSerialize for LockTime {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        // This implementation does not check the invariants on `LockTime` so that the
        // serialization is fallible only if the underlying writer is. This ensures that
        // we can always compute a hash of a transaction object.
        use LockTime::*;
        match self {
            Height(BlockHeight(n)) => writer.write_u32::<LittleEndian>(*n)?,
            Time(t) => writer.write_u32::<LittleEndian>(t.timestamp() as u32)?,
        }
        Ok(())
    }
}

impl ZcashDeserialize for LockTime {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let n = reader.read_u32::<LittleEndian>()?;
        if n < 500_000_000 {
            Ok(LockTime::Height(BlockHeight(n)))
        } else {
            Ok(LockTime::Time(Utc.timestamp(n as i64, 0)))
        }
    }
}

/// An encoding of a Bitcoin script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script(pub Vec<u8>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256d_checksum() {
        // https://en.bitcoin.it/wiki/Protocol_documentation#Hashes
        let input = b"hello";
        let checksum = Sha256dChecksum::from(&input[..]);
        let expected = Sha256dChecksum([0x95, 0x95, 0xc9, 0xdf]);
        assert_eq!(checksum, expected);
    }

    #[test]
    fn sha256d_checksum_debug() {
        let input = b"hello";
        let checksum = Sha256dChecksum::from(&input[..]);

        assert_eq!(format!("{:?}", checksum), "Sha256dChecksum(\"9595c9df\")");
    }
}
