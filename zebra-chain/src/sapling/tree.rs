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

#![allow(dead_code)]

use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
    ops::Deref,
};

use bitvec::prelude::*;
use incrementalmerkletree::{bridgetree, Frontier};
use lazy_static::lazy_static;
use thiserror::Error;

use super::commitment::pedersen_hashes::pedersen_hash;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};
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
#[derive(Clone, Copy, Default, Eq, Serialize, Deserialize)]
pub struct Root(#[serde(with = "serde_helpers::Fq")] pub(crate) jubjub::Base);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root")
            .field(&hex::encode(&self.0.to_bytes()))
            .finish()
    }
}

impl From<Root> for [u8; 32] {
    fn from(root: Root) -> Self {
        root.0.to_bytes()
    }
}

impl From<&Root> for [u8; 32] {
    fn from(root: &Root) -> Self {
        (*root).into()
    }
}

impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Hash for Root {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bytes().hash(state)
    }
}

impl TryFrom<[u8; 32]> for Root {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = jubjub::Base::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid jubjub::Base value for Sapling note commitment tree root",
            ))
        }
    }
}

impl ZcashSerialize for Root {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 32]>::from(*self)[..])?;

        Ok(())
    }
}

impl ZcashDeserialize for Root {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?)
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

impl serde::Serialize for Node {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Node {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = <[u8; 32]>::deserialize(deserializer)?;
        Option::<jubjub::Fq>::from(jubjub::Fq::from_bytes(&bytes))
            .map(Node::from)
            .ok_or_else(|| serde::de::Error::custom("invalid JubJub field element"))
    }
}

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum NoteCommitmentTreeError {
    #[error("The note commitment tree is full")]
    FullTree,
}

/// Sapling Incremental Note Commitment Tree.
#[derive(Debug, Serialize, Deserialize)]
pub struct NoteCommitmentTree {
    /// The tree represented as a Frontier.
    ///
    /// A Frontier is a subset of the tree that allows to fully specify it.
    /// It consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    ///
    /// # Consensus
    ///
    /// > [Sapling onward] A block MUST NOT add Sapling note commitments that would result in the Sapling note
    /// > commitment tree exceeding its capacity of 2^(MerkleDepth^Sapling) leaf nodes.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#merkletree>
    ///
    /// Note: MerkleDepth^Sapling = MERKLE_DEPTH = 32.
    inner: bridgetree::Frontier<Node, { MERKLE_DEPTH as u8 }>,

    /// A cached root of the tree.
    ///
    /// Every time the root is computed by [`Self::root`] it is cached here,
    /// and the cached value will be returned by [`Self::root`] until the tree is
    /// changed by [`Self::append`]. This greatly increases performance
    /// because it avoids recomputing the root when the tree does not change
    /// between blocks. In the finalized state, the tree is read from
    /// disk for every block processed, which would also require recomputing
    /// the root even if it has not changed (note that the cached root is
    /// serialized with the tree). This is particularly important since we decided
    /// to instantiate the trees from the genesis block, for simplicity.
    ///
    /// We use a [`RwLock`] for this cache, because it is only written once per tree update.
    /// Each tree has its own cached root, a new lock is created for each clone.
    cached_root: std::sync::RwLock<Option<Root>>,
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
            // Invalidate cached root
            let cached_root = self
                .cached_root
                .get_mut()
                .expect("a thread that previously held exclusive lock access panicked");

            *cached_root = None;

            Ok(())
        } else {
            Err(NoteCommitmentTreeError::FullTree)
        }
    }

    /// Returns the current root of the tree, used as an anchor in Sapling
    /// shielded transactions.
    pub fn root(&self) -> Root {
        if let Some(root) = self
            .cached_root
            .read()
            .expect("a thread that previously held exclusive lock access panicked")
            .deref()
        {
            // Return cached root.
            return *root;
        }

        // Get exclusive access, compute the root, and cache it.
        let mut write_root = self
            .cached_root
            .write()
            .expect("a thread that previously held exclusive lock access panicked");
        match write_root.deref() {
            // Another thread got write access first, return cached root.
            Some(root) => *root,
            None => {
                // Compute root and cache it.
                let root = Root::try_from(self.inner.root().0).unwrap();
                *write_root = Some(root);
                root
            }
        }
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
        self.inner.position().map_or(0, |pos| u64::from(pos) + 1)
    }
}

impl Clone for NoteCommitmentTree {
    /// Clones the inner tree, and creates a new `RwLock` with the cloned root data.
    fn clone(&self) -> Self {
        let cached_root = *self
            .cached_root
            .read()
            .expect("a thread that previously held exclusive lock access panicked");

        Self {
            inner: self.inner.clone(),
            cached_root: std::sync::RwLock::new(cached_root),
        }
    }
}

impl Default for NoteCommitmentTree {
    fn default() -> Self {
        Self {
            inner: bridgetree::Frontier::empty(),
            cached_root: Default::default(),
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
