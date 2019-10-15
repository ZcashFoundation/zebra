//! Definitions of block datastructures.

use chrono::{DateTime, Utc};
use hex;
use std::{fmt, io};

use crate::merkle_tree::MerkleTreeRootHash;
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::Transaction;

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
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct BlockHeaderHash(pub [u8; 32]);

impl fmt::Debug for BlockHeaderHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("BlockHeaderHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<BlockHeader> for BlockHeaderHash {
    fn from(block_header: BlockHeader) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        block_header
            .zcash_serialize(&mut hash_writer)
            .expect("Block headers must serialize.");
        Self(hash_writer.finish())
    }
}

impl ZcashSerialize for BlockHeaderHash {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        writer.write_all(&self.0)?;
        Ok(())
    }
}

impl ZcashDeserialize for BlockHeaderHash {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;
        Ok(BlockHeaderHash(bytes))
    }
}

/// Block header.
///
/// How are blocks chained together? They are chained together via the
/// backwards reference (previous header hash) present in the block
/// header. Each block points backwards to its parent, all the way
/// back to the genesis block (the first block in the blockchain).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockHeader {
    /// A SHA-256d hash in internal byte order of the previous block’s
    /// header. This ensures no previous block can be changed without
    /// also changing this block’s header.
    previous_block_hash: BlockHeaderHash,

    /// A SHA-256d hash in internal byte order. The merkle root is
    /// derived from the SHA256d hashes of all transactions included
    /// in this block as assembled in a binary tree, ensuring that
    /// none of those transactions can be modied without modifying the
    /// header.
    merkle_root_hash: MerkleTreeRootHash,

    /// [Sapling onward] The root LEBS2OSP256(rt) of the Sapling note
    /// commitment tree corresponding to the finnal Sapling treestate of
    /// this block.
    final_sapling_root_hash: SaplingNoteTreeRootHash,

    /// The block timestamp is a Unix epoch time (UTC) when the miner
    /// started hashing the header (according to the miner).
    time: DateTime<Utc>,

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
    bits: u32,

    /// An arbitrary field that miners can change to modify the header
    /// hash in order to produce a hash less than or equal to the
    /// target threshold.
    nonce: [u8; 32],

    /// The Equihash solution.
    // The solution size when serialized should be in bytes ('always
    // 1344').  I first tried this as a [u8; 1344] but until const
    // generics land we'd have to implement all our common traits
    // manually, like in pzec.
    solution: Vec<u8>,
}

impl ZcashSerialize for BlockHeader {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for BlockHeader {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

/// A block in your blockchain.
///
/// A block is a data structure with two fields:
///
/// Block header: a data structure containing the block's metadata
/// Transactions: an array (vector in Rust) of transactions
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// First 80 bytes of the block as defined by the encoding used by
    /// "block" messages.
    pub header: BlockHeader,

    /// The block transactions.
    pub transactions: Vec<Transaction>,
}

impl ZcashSerialize for Block {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for Block {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

#[cfg(test)]
mod tests {

    use std::io::Write;

    use super::BlockHeaderHash;

    use crate::sha256d_writer::Sha256dWriter;

    #[test]
    fn blockheaderhash_debug() {
        let preimage = b"foo bar baz";
        let mut sha_writer = Sha256dWriter::default();
        let _ = sha_writer.write_all(preimage);

        let hash = BlockHeaderHash(sha_writer.finish());

        assert_eq!(
            format!("{:?}", hash),
            "BlockHeaderHash(\"bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631\")"
        );
    }
}
