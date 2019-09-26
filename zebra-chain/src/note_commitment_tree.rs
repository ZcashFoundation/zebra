//! Note Commitment Trees.
//!
//! A note commitment tree is an incremental Merkle tree of fixed depth
//! used to store note commitments that JoinSplit transfers or Spend
//! transfers produce. Just as the unspent transaction output set (UTXO
//! set) used in Bitcoin, it is used to express the existence of value and
//! the capability to spend it. However, unlike the UTXO set, it is not
//! the job of this tree to protect against double-spending, as it is
//! append-only.
//!
//! A root of a note commitment tree is associated with each treestate.

use std::io;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

// XXX: Depending on if we implement SproutNoteCommitmentTree or
// similar, it may be worth it to define a NoteCommitmentTree trait.

/// Sapling Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaplingNoteCommitmentTree;

/// Sapling note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sapling note
/// commitment tree corresponding to the final Sapling treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaplingNoteTreeRootHash([u8; 32]);

impl From<SaplingNoteCommitmentTree> for SaplingNoteTreeRootHash {
    fn from(sapling_note_commitment_tree: SaplingNoteCommitmentTree) -> Self {
        // TODO: The Sapling note commitment tree requires a Pedersen
        // hash function, not SHA256.

        // let mut hash_writer = Sha256dWriter::default();
        // sapling_note_commitment_tree
        //     .zcash_serialize(&mut hash_writer)
        //     .expect("A Sapling note committment tree must serialize.");
        // Self(hash_writer.finish())

        unimplemented!();
    }
}

impl SaplingNoteCommitmentTree {
    /// Get the Jubjub-based Pedersen hash of root node of this merkle
    /// tree of commitment notes.
    pub fn hash(&self) -> [u8; 32] {
        unimplemented!();
    }
}

impl ZcashSerialize for SaplingNoteCommitmentTree {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for SaplingNoteCommitmentTree {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
