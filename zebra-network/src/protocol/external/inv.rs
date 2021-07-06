//! Inventory items for the Bitcoin protocol.

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
    /// A pair with the hash of a V5 transaction and the [Authorizing Data Commitment][auth_digest].
    ///
    /// Introduced by [ZIP-239][zip239], which is analogous to Bitcoin's [BIP-339][bip339].
    ///
    /// [auth_digest]: https://zips.z.cash/zip-0244#authorizing-data-commitment
    /// [zip239]: https://zips.z.cash/zip-0239
    /// [bip339]: https://github.com/bitcoin/bips/blob/master/bip-0339.mediawiki
    // TODO: Actually handle this variant once the mempool is implemented
    Wtx([u8; 64]),
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
        let (code, bytes): (_, &[u8]) = match self {
            InventoryHash::Error => (0, &[0; 32]),
            InventoryHash::Tx(hash) => (1, &hash.0),
            InventoryHash::Block(hash) => (2, &hash.0),
            InventoryHash::FilteredBlock(hash) => (3, &hash.0),
            InventoryHash::Wtx(bytes) => (5, bytes),
        };
        writer.write_u32::<LittleEndian>(code)?;
        writer.write_all(bytes)?;
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
            5 => {
                let auth_digest = reader.read_32_bytes()?;

                let mut wtx_bytes = [0u8; 64];
                wtx_bytes[..32].copy_from_slice(&bytes);
                wtx_bytes[32..].copy_from_slice(&auth_digest);

                Ok(InventoryHash::Wtx(wtx_bytes))
            }
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
