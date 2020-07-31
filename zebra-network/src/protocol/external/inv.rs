//! Inventory items for the Bitcoin protocol.

// XXX the exact optimal arrangement of all of these parts is a little unclear
// until we have more pieces in place the optimal global arrangement of items is
// a little unclear.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::serialization::{
    ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};
use zebra_chain::transaction::TransactionHash;

/// An inventory hash which refers to some advertised or requested data.
///
/// Bitcoin calls this an "inventory vector" but it is just a typed hash, not a
/// container, so we do not use that term to avoid confusion with `Vec<T>`.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Inventory_Vectors)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InventoryHash {
    /// An error.
    ///
    /// The Bitcoin wiki just says "Any data of with this number may be ignored",
    /// so we don't include a typed hash.
    Error,
    /// A hash of a transaction.
    Tx(TransactionHash),
    /// A hash of a block.
    Block(BlockHeaderHash),
    /// A hash of a filtered block.
    ///
    /// The Bitcoin wiki says: Hash of a block header, but only to be used in
    /// getdata message. Indicates the reply should be a merkleblock message
    /// rather than a block message; this only works if a bloom filter has been
    /// set.
    FilteredBlock(BlockHeaderHash),
}

impl From<TransactionHash> for InventoryHash {
    fn from(tx: TransactionHash) -> InventoryHash {
        InventoryHash::Tx(tx)
    }
}

impl From<BlockHeaderHash> for InventoryHash {
    fn from(block: BlockHeaderHash) -> InventoryHash {
        // Auto-convert to Block rather than FilteredBlock because filtered
        // blocks aren't useful for Zcash.
        InventoryHash::Block(block)
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
            1 => Ok(InventoryHash::Tx(TransactionHash(bytes))),
            2 => Ok(InventoryHash::Block(BlockHeaderHash(bytes))),
            3 => Ok(InventoryHash::FilteredBlock(BlockHeaderHash(bytes))),
            _ => Err(SerializationError::Parse("invalid inventory code")),
        }
    }
}
