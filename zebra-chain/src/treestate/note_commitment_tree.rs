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

#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use std::{fmt, io};

use bitvec::prelude::*;
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::{
    commitments::sapling::pedersen_hash_to_point,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// Pedersen Hash Function
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
fn pedersen_hash(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> jubjub::Fq {
    jubjub::AffinePoint::from(pedersen_hash_to_point(domain, M)).get_u()
}

/// MerkleCRH^Sapling Hash Function
///
/// MerkleCRH^Sapling(layer, left,right) := PedersenHash(“Zcash_PH”, l || left ||right)
/// where l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
// TODO: refine layer as a wrapper type around a bitvec/bitslice?
// TODO: refine output type as *NodeHash, combine with RootHash
fn merkle_crh_sapling(layer: u8, left: [u8; 32], right: [u8; 32]) -> jubjub::Fq {
    let mut s: BitVec<Lsb0, u8> = BitVec::new();

    // Prefix: l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
    s.append(&mut bitvec![31 - layer; 1]);
    s.append(&mut BitVec::<Lsb0, u8>::from_slice(&left[..]));
    s.append(&mut BitVec::<Lsb0, u8>::from_slice(&right[..]));

    pedersen_hash(*b"Zcash_PH", &s)
}

/// The index of a note’s commitment at the leafmost layer of its Note
/// Commitment Tree.
///
/// https://zips.z.cash/protocol/protocol.pdf#merkletree
pub struct Position(pub(crate) u64);

// XXX: Depending on if we implement SproutNoteCommitmentTree or
// similar, it may be worth it to define a NoteCommitmentTree trait.

/// Sapling Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct SaplingNoteCommitmentTree;

/// Sapling note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sapling note
/// commitment tree corresponding to the final Sapling treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct SaplingNoteTreeRootHash(pub [u8; 32]);

impl fmt::Debug for SaplingNoteTreeRootHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("SaplingNoteTreeRootHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<SaplingNoteCommitmentTree> for SaplingNoteTreeRootHash {
    fn from(_tree: SaplingNoteCommitmentTree) -> Self {
        // TODO: The Sapling note commitment tree requires a Pedersen
        // hash function, not SHA256.

        // let mut hash_writer = Sha256dWriter::default();
        // sapling_note_commitment_tree
        //     .zcash_serialize(&mut hash_writer)
        //     .expect("A Sapling note commitment tree must serialize.");
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
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), io::Error> {
        unimplemented!();
    }
}

impl ZcashDeserialize for SaplingNoteCommitmentTree {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
