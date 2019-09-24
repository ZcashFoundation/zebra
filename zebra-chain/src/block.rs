//! Definitions of block datastructures.

use crate::transaction::Transaction;
use chrono::{DateTime, Utc};

/// Block header
pub struct BlockHeader {
    /// A SHA-256d hash in internal byte order of the previous block’s
    /// header. This ensures no previous block can be changed without
    /// also changing this block’s header .
    previous_block_hash: [u8; 32],

    /// A SHA-256d hash in internal byte order. The merkle root is
    /// derived from the hashes of all transactions included in this
    /// block, ensuring that none of those transactions can be modied
    /// without modifying the header.
    merkle_root_hash: [u8; 32],

    /// [Sapling onward] The root LEBS2OSP256(rt) of the Sapling note
    /// commitment tree corresponding to the nal Sapling treestate of
    /// this block.
    // TODO: replace type with custom SaplingRoot or similar type
    // hash_final_sapling_root: [u8; 32],

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

/// A block in your blockchain.
pub struct Block {
    /// First 80 bytes of the block as defined by the encoding used by
    /// "block" messages
    pub header: BlockHeader,

    ///
    pub transactions: Vec<Transaction>,
}
