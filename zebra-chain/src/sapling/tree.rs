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

use std::fmt;

use bitvec::prelude::*;
use incrementalmerkletree::{bridgetree, Frontier};
use lazy_static::lazy_static;
use thiserror::Error;

use super::commitment::pedersen_hashes::pedersen_hash;

pub(super) const MERKLE_DEPTH: usize = 32;

/// MerkleCRH^Sapling Hash Function
///
/// Used to hash incremental Merkle tree hash values for Sapling.
///
/// MerkleCRH^Sapling(layer, left, right) := PedersenHash("Zcash_PH", l || left || right)
/// where l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer) and
/// left, right, and the output are all technically 255 bits (l_MerkleSapling), not 256.
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
fn merkle_crh_sapling(layer: u8, left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut s = bitvec![Lsb0, u8;];

    // Prefix: l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
    let l = (MERKLE_DEPTH - 1) as u8 - layer;
    s.extend_from_bitslice(&BitSlice::<Lsb0, _>::from_element(&l)[0..6]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(left)[0..255]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(right)[0..255]);

    pedersen_hash(*b"Zcash_PH", &s).to_bytes()
}

lazy_static! {
    /// List of "empty" Sapling note commitment nodes, one for each layer.
    ///
    /// The list is indexed by the layer number (0: root; MERKLE_DEPTH: leaf).
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#constants
    pub(super) static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // The empty leaf node. This is layer 32.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for layer in (0..MERKLE_DEPTH).rev() {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            let next = merkle_crh_sapling(layer as u8, v[0], v[0]);
            v.insert(0, next);
        }

        v

    };
}

/// The index of a note's commitment at the leafmost layer of its Note
/// Commitment Tree.
///
/// https://zips.z.cash/protocol/protocol.pdf#merkletree
pub struct Position(pub(crate) u64);

/// Sapling note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sapling note
/// commitment tree corresponding to the final Sapling treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct Root(pub [u8; 32]);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(&self.0)).finish()
    }
}

impl From<[u8; 32]> for Root {
    fn from(bytes: [u8; 32]) -> Root {
        Self(bytes)
    }
}

impl From<Root> for [u8; 32] {
    fn from(root: Root) -> Self {
        root.0
    }
}

impl From<&[u8; 32]> for Root {
    fn from(bytes: &[u8; 32]) -> Root {
        (*bytes).into()
    }
}

impl From<&Root> for [u8; 32] {
    fn from(root: &Root) -> Self {
        (*root).into()
    }
}

/// A node of the Sapling Incremental Note Commitment Tree.
///
/// Note that it's handled as a byte buffer and not a point coordinate (jubjub::Fq)
/// because that's how the spec handles the MerkleCRH^Sapling function inputs and outputs.
#[derive(Clone, Debug)]
struct Node([u8; 32]);

impl incrementalmerkletree::Hashable for Node {
    fn empty_leaf() -> Self {
        Self(NoteCommitmentTree::uncommitted())
    }

    /// Combine two nodes to generate a new node in the given level.
    /// Level 0 is the layer above the leaves (layer 31).
    /// Level 31 is the root (layer 0).
    fn combine(level: incrementalmerkletree::Altitude, a: &Self, b: &Self) -> Self {
        let layer = (MERKLE_DEPTH - 1) as u8 - u8::from(level);
        Self(merkle_crh_sapling(layer, a.0, b.0))
    }

    /// Return the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Altitude) -> Self {
        let layer_below: usize = MERKLE_DEPTH - usize::from(level);
        Self(EMPTY_ROOTS[layer_below])
    }
}

impl From<jubjub::Fq> for Node {
    fn from(x: jubjub::Fq) -> Self {
        Node(x.into())
    }
}

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, PartialEq)]
pub enum NoteCommitmentTreeError {
    #[error("The note commitment tree is full")]
    FullTree,
}

/// Sapling Incremental Note Commitment Tree.
#[derive(Clone, Debug)]
pub struct NoteCommitmentTree {
    /// The tree represented as a Frontier.
    ///
    /// A Frontier is a subset of the tree that allows to fully specify it.
    /// It consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    inner: bridgetree::Frontier<Node, { MERKLE_DEPTH as u8 }>,
}

impl NoteCommitmentTree {
    /// Adds a note commitment u-coordinate to the tree.
    ///
    /// The leaves of the tree are actually a base field element, the
    /// u-coordinate of the commitment, the data that is actually stored on the
    /// chain and input into the proof.
    ///
    /// Returns an error if the tree is full.
    pub fn append(&mut self, cm_u: jubjub::Fq) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(&cm_u.into()) {
            Ok(())
        } else {
            Err(NoteCommitmentTreeError::FullTree)
        }
    }

    /// Returns the current root of the tree, used as an anchor in Sapling
    /// shielded transactions.
    pub fn root(&self) -> Root {
        Root(self.inner.root().0)
    }

    /// Get the Jubjub-based Pedersen hash of root node of this merkle tree of
    /// note commitments.
    pub fn hash(&self) -> [u8; 32] {
        self.root().into()
    }

    /// An as-yet unused Sapling note commitment tree leaf node.
    ///
    /// Distinct for Sapling, a distinguished hash value of:
    ///
    /// Uncommitted^Sapling = I2LEBSP_l_MerkleSapling(1)
    pub fn uncommitted() -> [u8; 32] {
        jubjub::Fq::one().to_bytes()
    }

    /// Count of note commitments added to the tree.
    ///
    /// For Sapling, the tree is capped at 2^32.
    pub fn count(&self) -> u64 {
        self.inner
            .position()
            .map_or(0, |pos| usize::from(pos) as u64 + 1)
    }
}

impl Default for NoteCommitmentTree {
    fn default() -> Self {
        Self {
            inner: bridgetree::Frontier::new(),
        }
    }
}

impl Eq for NoteCommitmentTree {}

impl PartialEq for NoteCommitmentTree {
    fn eq(&self, other: &Self) -> bool {
        self.hash() == other.hash()
    }
}

impl From<Vec<jubjub::Fq>> for NoteCommitmentTree {
    /// Compute the tree from a whole bunch of note commitments at once.
    fn from(values: Vec<jubjub::Fq>) -> Self {
        let mut tree = Self::default();

        if values.is_empty() {
            return tree;
        }

        for cm_u in values {
            let _ = tree.append(cm_u);
        }

        tree
    }
}
