//! Newtype wrappers for primitive data types with semantic meaning.

use hex;
use std::fmt;

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
