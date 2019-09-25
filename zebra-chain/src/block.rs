//! Definitions of block datastructures.

use chrono::{DateTime, Utc};
use std::io;

use crate::merkle_tree::MerkleTree;
use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::Transaction;

/// A SHA-256d hash of a BlockHeader.
///
/// This is useful when one block header is pointing to its parent
/// block header in the block chain. ⛓️
pub struct BlockHash([u8; 32]);

impl From<BlockHeader> for BlockHash {
    fn from(block_header: BlockHeader) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        block_header
            .zcash_serialize(&mut hash_writer)
            .expect("Block headers must serialize.");
        Self(hash_writer.finish())
    }
}

/// A SHA-256d hash of the root node of a merkle tree of SHA256-d
/// hashed transactions in a block.
pub struct MerkleRootHash([u8; 32]);

impl<Transaction> ZcashSerialize for MerkleTree<Transaction> {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl<Transaction> ZcashDeserialize for MerkleTree<Transaction> {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

impl From<MerkleTree<Transaction>> for MerkleRootHash {
    fn from(merkle_tree: MerkleTree<Transaction>) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        merkle_tree
            .zcash_serialize(&mut hash_writer)
            .expect("The merkle tree of transactions must serialize.");
        Self(hash_writer.finish())
    }
}

/// Block header
pub struct BlockHeader {
    /// A SHA-256d hash in internal byte order of the previous block’s
    /// header. This ensures no previous block can be changed without
    /// also changing this block’s header .
    previous_block_hash: BlockHash,

    /// A SHA-256d hash in internal byte order. The merkle root is
    /// derived from the SHA256d hashes of all transactions included
    /// in this block as assembled in a binary tree, ensuring that
    /// none of those transactions can be modied without modifying the
    /// header.
    merkle_root_hash: [u8; 32],

    /// [Sapling onward] The root LEBS2OSP256(rt) of the Sapling note
    /// commitment tree corresponding to the nal Sapling treestate of
    /// this block.
    // TODO: replace type with custom SaplingRoot or similar type
    // hash_final_sapling_root: SaplingRootHash,

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
    // The solution size when serialized should be in bytes ('always 1344').
    solution: [u8; 1344],
}

impl BlockHeader {
    /// Get the SHA-256d hash in internal byte order of this block header.
    pub fn hash(&self) -> [u8; 32] {
        unimplemented!();
    }
}

impl ZcashSerialize for BlockHeader {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for BlockHeader {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

/// A block in your blockchain.
pub struct Block {
    /// First 80 bytes of the block as defined by the encoding used by
    /// "block" messages.
    pub header: BlockHeader,

    /// Block transactions.
    pub transactions: Vec<Transaction>,
}
