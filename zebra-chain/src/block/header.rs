use chrono::{DateTime, Utc};

use crate::equihash_solution::EquihashSolution;
use crate::merkle_tree::MerkleTreeRootHash;
use crate::note_commitment_tree::SaplingNoteTreeRootHash;

use super::BlockHeaderHash;

/// Block header.
///
/// How are blocks chained together? They are chained together via the
/// backwards reference (previous header hash) present in the block
/// header. Each block points backwards to its parent, all the way
/// back to the genesis block (the first block in the blockchain).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// The block's version field. This is supposed to be `4`:
    ///
    /// > The current and only defined block version number for Zcash is 4.
    ///
    /// but this was not enforced by the consensus rules, and defective mining
    /// software created blocks with other versions, so instead it's effectively
    /// a free field. The only constraint is that it must be at least `4` when
    /// interpreted as an `i32`.
    pub version: u32,

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
    /// commitment tree corresponding to the final Sapling treestate of
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

impl BlockHeader {
    /// Returns true if the header is valid based on its `EquihashSolution`
    pub fn is_equihash_solution_valid(&self) -> Result<(), Error> {
        use crate::serialization::ZcashSerialize;

        let nonce = &self.nonce;
        let solution = &self.solution;
        let mut input = Vec::new();

        self.zcash_serialize(&mut input)?;

        solution.is_valid(&input.as_slice()[0..EquihashSolution::INPUT_LENGTH], nonce)?;

        Ok(())
    }
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    EquihashInvalid(#[from] crate::equihash_solution::Error),
    #[error("cannot reserialize header for verification")]
    Serialize(#[from] std::io::Error),
}
