//! Inventory items for the Bitcoin protocol.

// XXX the exact optimal arrangement of all of these parts is a little unclear
// until we have more pieces in place the optimal global arrangement of items is
// a little unclear.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::{
    block,
    serialization::{
        ReadZcashExt, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
    transaction,
};

use super::MAX_PROTOCOL_MESSAGE_LEN;

/// An inventory hash which refers to some advertised or requested data.
///
/// Bitcoin calls this an "inventory vector" but it is just a typed hash, not a
/// container, so we do not use that term to avoid confusion with `Vec<T>`.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Inventory_Vectors)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum InventoryHash {
    /// An error.
    ///
    /// The Bitcoin wiki just says "Any data of with this number may be ignored",
    /// so we don't include a typed hash.
    Error,
    /// A hash of a transaction.
    Tx(transaction::Hash),
    /// A hash of a block.
    Block(block::Hash),
    /// A hash of a filtered block.
    ///
    /// The Bitcoin wiki says: Hash of a block header, but only to be used in
    /// getdata message. Indicates the reply should be a merkleblock message
    /// rather than a block message; this only works if a bloom filter has been
    /// set.
    FilteredBlock(block::Hash),
}

impl From<transaction::Hash> for InventoryHash {
    fn from(tx: transaction::Hash) -> InventoryHash {
        InventoryHash::Tx(tx)
    }
}

impl From<block::Hash> for InventoryHash {
    fn from(hash: block::Hash) -> InventoryHash {
        // Auto-convert to Block rather than FilteredBlock because filtered
        // blocks aren't useful for Zcash.
        InventoryHash::Block(hash)
    }
}

impl ZcashSerialize for InventoryHash {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        let (code, bytes) = match *self {
            InventoryHash::Error => (0, [0; 32]),
            InventoryHash::Tx(hash) => (1, hash.0),
            InventoryHash::Block(hash) => (2, hash.0),
            InventoryHash::FilteredBlock(hash) => (3, hash.0),
        };
        writer.write_u32::<LittleEndian>(code)?;
        writer.write_all(&bytes)?;
        Ok(())
    }
}

impl ZcashDeserialize for InventoryHash {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let code = reader.read_u32::<LittleEndian>()?;
        let bytes = reader.read_32_bytes()?;
        match code {
            0 => Ok(InventoryHash::Error),
            1 => Ok(InventoryHash::Tx(transaction::Hash(bytes))),
            2 => Ok(InventoryHash::Block(block::Hash(bytes))),
            3 => Ok(InventoryHash::FilteredBlock(block::Hash(bytes))),
            _ => Err(SerializationError::Parse("invalid inventory code")),
        }
    }
}

const INV_HASH_SIZE: usize = 36;
impl TrustedPreallocate for InventoryHash {
    fn max_allocation() -> u64 {
        // An Inventory hash takes 36 bytes, and we reserve at least one byte for the Vector length
        // so we can never receive more than ((MAX_PROTOCOL_MESSAGE_LEN - 1) / 36) in a single message
        ((MAX_PROTOCOL_MESSAGE_LEN - 1) / INV_HASH_SIZE) as u64
    }
}

#[cfg(test)]
mod test_trusted_preallocate {
    use std::convert::TryInto;

    use super::{InventoryHash, INV_HASH_SIZE, MAX_PROTOCOL_MESSAGE_LEN};
    use zebra_chain::{
        block,
        serialization::{TrustedPreallocate, ZcashSerialize},
        transaction,
    };
    #[test]
    /// Confirm that each InventoryHash takes exactly INV_HASH_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    fn inv_hash_size_is_correct() {
        let block_hash = block::Hash([1u8; 32]);
        let tx_hash = transaction::Hash([1u8; 32]);
        let inv_block = InventoryHash::Block(block_hash);
        let serialized_inv_block = inv_block
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized_inv_block.len() == INV_HASH_SIZE);

        let inv_filtered_block = InventoryHash::FilteredBlock(block_hash);
        let serialized_inv_filtered = inv_filtered_block
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized_inv_filtered.len() == INV_HASH_SIZE);

        let inv_tx = InventoryHash::Tx(tx_hash);
        let serialized_inv_tx = inv_tx
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serialized_inv_tx.len() == INV_HASH_SIZE);

        let inv_err = InventoryHash::Error;
        let serializd_inv_err = inv_err
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        assert!(serializd_inv_err.len() == INV_HASH_SIZE)
    }
    #[test]
    /// Verifies that...
    /// 1. The smallest disallowed vector of `InventoryHash`s is too large to fit in a legal Zcash message
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash message
    fn meta_addr_max_allocation_is_correct() {
        let inv = InventoryHash::Error;
        let max_allocation: usize = InventoryHash::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(InventoryHash::max_allocation() + 1) {
            smallest_disallowed_vec.push(inv);
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        assert!(((smallest_disallowed_vec.len() - 1) as u64) == InventoryHash::max_allocation());
        // Check that our smallest_disallowed_vec is too big to fit in a Zcash message.
        assert!(smallest_disallowed_serialized.len() > MAX_PROTOCOL_MESSAGE_LEN);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec
            .zcash_serialize_to_vec()
            .expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of InventoryHashes
        assert!((largest_allowed_vec.len() as u64) == InventoryHash::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash message.
        assert!(largest_allowed_serialized.len() <= MAX_PROTOCOL_MESSAGE_LEN);
    }
}
