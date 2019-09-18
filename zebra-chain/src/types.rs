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

    /// Hash of a block header, but only to be used in getdata
    /// message. Indicates the reply should be a cmpctblock
    /// message. See
    /// [BIP-152](https://github.com/bitcoin/bips/blob/master/bip-0152.mediawiki)
    /// for more info.
    // XXX We may not need this, pzec does not.
    MsgCmpctBlock = 0x04,
}

/// Inventory Vector
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InventoryVector(pub InventoryType, pub [u8; 32]);

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutPoint {
    /// The hash of the referenced transaction.
    pub hash: [u8; 32],

    /// The index of the specific output in the transaction. The first output is 0, etc.
    pub index: u32,
}

/// Transaction Input
// `Copy` cannot be implemented for `Vec<u8>`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionInput {
    /// The previous output transaction reference.
    pub previous_output: OutPoint,

    /// Computational Script for confirming transaction authorization.
    // XXX pzec uses their own `Bytes` type that wraps a `Vec<u8>`
    // with some extra methods.
    pub signature_script: Vec<u8>,

    /// Transaction version as defined by the sender. Intended for
    /// "replacement" of transactions when information is updated
    /// before inclusion into a block.
    pub sequence: u32,
}

/// Transaction Output
// `Copy` cannot be implemented for `Vec<u8>`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionOutput {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    pub value: u64,

    /// Usually contains the public key as a Bitcoin script setting up
    /// conditions to claim this output.
    pub pk_script: Vec<u8>,
}

/// Transaction Input
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transaction {
    /// Transaction data format version (note, this is signed).
    pub version: i32,

    /// A list of 1 or more transaction inputs or sources for coins.
    pub tx_in: Vec<TransactionInput>,

    /// A list of 1 or more transaction outputs or destinations for coins.
    pub tx_out: Vec<TransactionOutput>,

    /// The block number or timestamp at which this transaction is unlocked:
    ///
    /// |Value       |Description                                         |
    /// |------------|----------------------------------------------------|
    /// |0           |Not locked (default)                                |
    /// |< 500000000 |Block number at which this transaction is unlocked  |
    /// |>= 500000000|UNIX timestamp at which this transaction is unlocked|
    ///
    /// If all `TransactionInput`s have final (0xffffffff) sequence
    /// numbers, then lock_time is irrelevant. Otherwise, the
    /// transaction may not be added to a block until after `lock_time`.
    pub lock_time: u32,
}

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
