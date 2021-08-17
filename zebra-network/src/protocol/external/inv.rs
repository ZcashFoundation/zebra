//! Inventory items for the Bitcoin protocol.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::{
    block,
    serialization::{
        ReadZcashExt, SerializationError, TrustedPreallocate, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
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
    /// A pair with the hash of a V5 transaction and the [Authorizing Data Commitment][auth_digest].
    ///
    /// Introduced by [ZIP-239][zip239], which is analogous to Bitcoin's [BIP-339][bip339].
    ///
    /// [auth_digest]: https://zips.z.cash/zip-0244#authorizing-data-commitment
    /// [zip239]: https://zips.z.cash/zip-0239
    /// [bip339]: https://github.com/bitcoin/bips/blob/master/bip-0339.mediawiki
    // TODO: Actually handle this variant once the mempool is implemented (#2449)
    Wtx(transaction::WtxId),
}

impl InventoryHash {
    /// Returns the serialized Zcash network protocol code for the current variant.
    fn code(&self) -> u32 {
        match self {
            InventoryHash::Error => 0,
            InventoryHash::Tx(_tx_id) => 1,
            InventoryHash::Block(_hash) => 2,
            InventoryHash::FilteredBlock(_hash) => 3,
            InventoryHash::Wtx(_wtx_id) => 5,
        }
    }
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

impl From<transaction::WtxId> for InventoryHash {
    fn from(wtx_id: transaction::WtxId) -> InventoryHash {
        InventoryHash::Wtx(wtx_id)
    }
}

impl ZcashSerialize for InventoryHash {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(self.code())?;
        match self {
            // Zebra does not supply error codes
            InventoryHash::Error => writer.write_all(&[0; 32]),
            InventoryHash::Tx(tx_id) => tx_id.zcash_serialize(writer),
            InventoryHash::Block(hash) => hash.zcash_serialize(writer),
            InventoryHash::FilteredBlock(hash) => hash.zcash_serialize(writer),
            InventoryHash::Wtx(wtx_id) => wtx_id.zcash_serialize(writer),
        }
    }
}

impl ZcashDeserialize for InventoryHash {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let code = reader.read_u32::<LittleEndian>()?;
        match code {
            0 => {
                // ignore the standard 32-byte error code
                let _bytes = reader.read_32_bytes()?;
                Ok(InventoryHash::Error)
            }

            1 => Ok(InventoryHash::Tx(reader.zcash_deserialize_into()?)),
            2 => Ok(InventoryHash::Block(reader.zcash_deserialize_into()?)),
            3 => Ok(InventoryHash::FilteredBlock(
                reader.zcash_deserialize_into()?,
            )),
            5 => Ok(InventoryHash::Wtx(reader.zcash_deserialize_into()?)),

            _ => Err(SerializationError::Parse("invalid inventory code")),
        }
    }
}

/// The minimum serialized size of an [`InventoryHash`].
pub(crate) const MIN_INV_HASH_SIZE: usize = 36;

impl TrustedPreallocate for InventoryHash {
    fn max_allocation() -> u64 {
        // An Inventory hash takes at least 36 bytes, and we reserve at least one byte for the
        // Vector length so we can never receive more than ((MAX_PROTOCOL_MESSAGE_LEN - 1) / 36) in
        // a single message
        ((MAX_PROTOCOL_MESSAGE_LEN - 1) / MIN_INV_HASH_SIZE) as u64
    }
}
