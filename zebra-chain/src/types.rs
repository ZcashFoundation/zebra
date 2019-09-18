//! Newtype wrappers for primitive data types with semantic meaning.

/// A 4-byte checksum using truncated double SHA256.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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

/// A u32 which represents a block height value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockHeight(pub u32);

/// InventoryType
///
/// [Bitcoin·reference](https://en.bitcoin.it/wiki/Protocol_documentation#Inventory_Vectors)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InventoryType {
    /// Any data of with this number may be ignored.
    Error = 0x00,

    /// Hash is related to a transaction.
    MsgTx = 0x01,

    /// Hash is related to a data block.
    MsgBlock = 0x02,

    /// Hash of a block header, but only to be used in getdata
    /// message. Indicates the reply should be a merkleblock message
    /// rather than a block message; this only works if a bloom filter
    /// has been set.
    // XXX: Since we don't intend to include the bloom filter to
    // start, do we need this?
    MsgFilteredBlock = 0x03,
}

/// Inventory Vector
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InventoryVector(pub InventoryType, pub [u8; 32]);

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
}
