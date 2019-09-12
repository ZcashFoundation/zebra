//! Newtype wrappers for primitive data types with semantic meaning.

/// A u32 which represents a block height value.
pub struct BlockHeight(pub u32);

/// InventoryType
///
/// [Bitcoin·reference](https://en.bitcoin.it/wiki/Protocol_documentation#Inventory_Vectors)
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
    MsgCmpctBlock = 0x04,
}

/// Inventory Vector
pub struct InventoryVector(pub InventoryType, pub [u8; 32]);
