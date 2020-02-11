//! Definitions of block datastructures.

#[cfg(test)]
pub mod test_vectors;
#[cfg(test)]
mod tests;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use std::{fmt, io};

#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::equihash_solution::EquihashSolution;
use crate::merkle_tree::MerkleTreeRootHash;
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::Transaction;
use crate::types::BlockHeight;

/// A SHA-256d hash of a BlockHeader.
///
/// This is useful when one block header is pointing to its parent
/// block header in the block chain. ⛓️
///
/// This is usually called a 'block hash', as it is frequently used
/// to identify the entire block, since the hash preimage includes
/// the merkle root of the transactions in this block. But
/// _technically_, this is just a hash of the block _header_, not
/// the direct bytes of the transactions as well as the header. So
/// for now I want to call it a `BlockHeaderHash` because that's
/// more explicit.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct BlockHeaderHash(pub [u8; 32]);

impl fmt::Debug for BlockHeaderHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("BlockHeaderHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl<'a> From<&'a BlockHeader> for BlockHeaderHash {
    fn from(block_header: &'a BlockHeader) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        block_header
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finish())
    }
}

impl ZcashSerialize for BlockHeaderHash {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)?;
        Ok(())
    }
}

impl ZcashDeserialize for BlockHeaderHash {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(BlockHeaderHash(reader.read_32_bytes()?))
    }
}

/// Block header.
///
/// How are blocks chained together? They are chained together via the
/// backwards reference (previous header hash) present in the block
/// header. Each block points backwards to its parent, all the way
/// back to the genesis block (the first block in the blockchain).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockHeader {
    /// A SHA-256d hash in internal byte order of the previous block’s
    /// header. This ensures no previous block can be changed without
    /// also changing this block’s header.
    pub previous_block_hash: BlockHeaderHash,

    /// A SHA-256d hash in internal byte order. The merkle root is
    /// derived from the SHA256d hashes of all transactions included
    /// in this block as assembled in a binary tree, ensuring that
    /// none of those transactions can be modied without modifying the
    /// header.
    pub merkle_root_hash: MerkleTreeRootHash,

    /// [Sapling onward] The root LEBS2OSP256(rt) of the Sapling note
    /// commitment tree corresponding to the finnal Sapling treestate of
    /// this block.
    pub final_sapling_root_hash: SaplingNoteTreeRootHash,

    /// The block timestamp is a Unix epoch time (UTC) when the miner
    /// started hashing the header (according to the miner).
    pub time: DateTime<Utc>,

    /// An encoded version of the target threshold this block’s header
    /// hash must be less than or equal to, in the same nBits format
    /// used by Bitcoin.
    ///
    /// For a block at block height height, bits MUST be equal to
    /// ThresholdBits(height).
    ///
    /// [Bitcoin-nBits](https://bitcoin.org/en/developer-reference#target-nbits)
    // pzec has their own wrapper around u32 for this field:
    // https://github.com/ZcashFoundation/zebra/blob/master/zebra-primitives/src/compact.rs
    pub bits: u32,

    /// An arbitrary field that miners can change to modify the header
    /// hash in order to produce a hash less than or equal to the
    /// target threshold.
    pub nonce: [u8; 32],

    /// The Equihash solution.
    pub solution: EquihashSolution,
}

impl ZcashSerialize for BlockHeader {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // "The current and only defined block version number for Zcash is 4."
        writer.write_u32::<LittleEndian>(4)?;
        self.previous_block_hash.zcash_serialize(&mut writer)?;
        writer.write_all(&self.merkle_root_hash.0[..])?;
        writer.write_all(&self.final_sapling_root_hash.0[..])?;
        writer.write_u32::<LittleEndian>(self.time.timestamp() as u32)?;
        writer.write_u32::<LittleEndian>(self.bits)?;
        writer.write_all(&self.nonce[..])?;
        self.solution.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for BlockHeader {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // "The current and only defined block version number for Zcash is 4."
        let version = reader.read_u32::<LittleEndian>()?;

        if version != 4 {
            return Err(SerializationError::Parse("bad block header"));
        }

        Ok(BlockHeader {
            previous_block_hash: BlockHeaderHash::zcash_deserialize(&mut reader)?,
            merkle_root_hash: MerkleTreeRootHash(reader.read_32_bytes()?),
            final_sapling_root_hash: SaplingNoteTreeRootHash(reader.read_32_bytes()?),
            time: Utc.timestamp(reader.read_u32::<LittleEndian>()? as i64, 0),
            bits: reader.read_u32::<LittleEndian>()?,
            nonce: reader.read_32_bytes()?,
            solution: EquihashSolution::zcash_deserialize(reader)?,
        })
    }
}

/// A Zcash block, containing a [`BlockHeader`] and a sequence of
/// [`Transaction`]s.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Block {
    /// The block header, containing block metadata.
    pub header: BlockHeader,
    /// The block transactions.
    pub transactions: Vec<Transaction>,
}

impl Block {
    /// Return the block height reported in the coinbase transaction, if any.
    pub fn coinbase_height(&self) -> Option<BlockHeight> {
        use crate::transaction::TransparentInput;
        self.transactions
            .get(0)
            .and_then(|tx| tx.inputs().next())
            .and_then(|input| match input {
                TransparentInput::Coinbase { ref height, .. } => Some(*height),
                _ => None,
            })
    }
}

impl<'a> From<&'a Block> for BlockHeaderHash {
    fn from(block: &'a Block) -> BlockHeaderHash {
        (&block.header).into()
    }
}

impl ZcashSerialize for Block {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.header.zcash_serialize(&mut writer)?;
        self.transactions.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Block {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Block {
            header: BlockHeader::zcash_deserialize(&mut reader)?,
            transactions: Vec::zcash_deserialize(&mut reader)?,
        })
    }
}
