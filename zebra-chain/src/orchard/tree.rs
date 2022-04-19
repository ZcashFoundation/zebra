//! Note Commitment Trees.
//!
//! A note commitment tree is an incremental Merkle tree of fixed depth
//! used to store note commitments that Action
//! transfers produce. Just as the unspent transaction output set (UTXO
//! set) used in Bitcoin, it is used to express the existence of value and
//! the capability to spend it. However, unlike the UTXO set, it is not
//! the job of this tree to protect against double-spending, as it is
//! append-only.
//!
//! A root of a note commitment tree is associated with each treestate.

#![allow(clippy::derive_hash_xor_eq)]
#![allow(dead_code)]

use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
    ops::Deref,
};

use bitvec::prelude::*;
use halo2::pasta::{group::ff::PrimeField, pallas};
use incrementalmerkletree::{bridgetree, Frontier};
use lazy_static::lazy_static;
use thiserror::Error;

use super::sinsemilla::*;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

pub(super) const MERKLE_DEPTH: usize = 32;

/// MerkleCRH^Orchard Hash Function
///
/// Used to hash incremental Merkle tree hash values for Orchard.
///
/// MerkleCRH^Orchard: {0..MerkleDepth^Orchard − 1} × P𝑥 × P𝑥 → P𝑥
///
/// MerkleCRH^Orchard(layer, left, right) := 0 if hash == ⊥; hash otherwise
///
/// where hash = SinsemillaHash("z.cash:Orchard-MerkleCRH", l || left || right),
/// l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer),  and left, right, and
/// the output are the x-coordinates of Pallas affine points.
///
/// https://zips.z.cash/protocol/protocol.pdf#orchardmerklecrh
/// https://zips.z.cash/protocol/protocol.pdf#constants
fn merkle_crh_orchard(layer: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let mut s = bitvec![Lsb0, u8;];

    // Prefix: l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer)
    let l = MERKLE_DEPTH - 1 - layer as usize;
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from([l, 0])[0..10]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(left.to_repr())[0..255]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(right.to_repr())[0..255]);

    match sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &s) {
        Some(h) => h,
        None => pallas::Base::zero(),
    }
}

lazy_static! {
    /// List of "empty" Orchard note commitment nodes, one for each layer.
    ///
    /// The list is indexed by the layer number (0: root; MERKLE_DEPTH: leaf).
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#constants
    pub(super) static ref EMPTY_ROOTS: Vec<pallas::Base> = {
        // The empty leaf node. This is layer 32.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for layer in (0..MERKLE_DEPTH).rev()
        {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            let next = merkle_crh_orchard(layer as u8, v[0], v[0]);
            v.insert(0, next);
        }

        v

    };
}

/// Orchard note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Orchard note commitment
/// tree corresponding to the final Orchard treestate of this block. A root of a
/// note commitment tree is associated with each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Root(#[serde(with = "serde_helpers::Base")] pub(crate) pallas::Base);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root")
            .field(&hex::encode(&self.0.to_repr()))
            .finish()
    }
}

impl From<Root> for [u8; 32] {
    fn from(root: Root) -> Self {
        root.0.into()
    }
}

impl From<&Root> for [u8; 32] {
    fn from(root: &Root) -> Self {
        (*root).into()
    }
}

impl Hash for Root {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_repr().hash(state)
    }
}

impl TryFrom<[u8; 32]> for Root {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Base::from_repr(bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for Orchard note commitment tree root",
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

/// A node of the Orchard Incremental Note Commitment Tree.
#[derive(Clone, Debug)]
struct Node(pallas::Base);

impl incrementalmerkletree::Hashable for Node {
    fn empty_leaf() -> Self {
        Self(NoteCommitmentTree::uncommitted())
    }

    /// Combine two nodes to generate a new node in the given level.
    /// Level 0 is the layer above the leaves (layer 31).
    /// Level 31 is the root (layer 0).
    fn combine(level: incrementalmerkletree::Altitude, a: &Self, b: &Self) -> Self {
        let layer = (MERKLE_DEPTH - 1) as u8 - u8::from(level);
        Self(merkle_crh_orchard(layer, a.0, b.0))
    }

    /// Return the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Altitude) -> Self {
        let layer_below: usize = MERKLE_DEPTH - usize::from(level);
        Self(EMPTY_ROOTS[layer_below])
    }
}

impl From<pallas::Base> for Node {
    fn from(x: pallas::Base) -> Self {
        Node(x)
    }
}

impl serde::Serialize for Node {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.to_repr().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Node {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes = <[u8; 32]>::deserialize(deserializer)?;
        Option::<pallas::Base>::from(pallas::Base::from_repr(bytes))
            .map(Node)
            .ok_or_else(|| serde::de::Error::custom("invalid Pallas field element"))
    }
}

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum NoteCommitmentTreeError {
    #[error("The note commitment tree is full")]
    FullTree,
}

/// Orchard Incremental Note Commitment Tree
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
    /// > [NU5 onward] A block MUST NOT add Orchard note commitments that would result in the Orchard note
    /// > commitment tree exceeding its capacity of 2^(MerkleDepth^Orchard) leaf nodes.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#merkletree>
    ///
    /// Note: MerkleDepth^Orchard = MERKLE_DEPTH = 32.
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
    /// Adds a note commitment x-coordinate to the tree.
    ///
    /// The leaves of the tree are actually a base field element, the
    /// x-coordinate of the commitment, the data that is actually stored on the
    /// chain and input into the proof.
    ///
    /// Returns an error if the tree is full.
    pub fn append(&mut self, cm_x: pallas::Base) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(&cm_x.into()) {
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

    /// Returns the current root of the tree, used as an anchor in Orchard
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
                let root = Root(self.inner.root().0);
                *write_root = Some(root);
                root
            }
        }
    }

    /// Get the Pallas-based Sinsemilla hash / root node of this merkle tree of
    /// note commitments.
    pub fn hash(&self) -> [u8; 32] {
        self.root().into()
    }

    /// An as-yet unused Orchard note commitment tree leaf node.
    ///
    /// Distinct for Orchard, a distinguished hash value of:
    ///
    /// Uncommitted^Orchard = I2LEBSP_l_MerkleOrchard(2)
    pub fn uncommitted() -> pallas::Base {
        pallas::Base::one().double()
    }

    /// Count of note commitments added to the tree.
    ///
    /// For Orchard, the tree is capped at 2^32.
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

impl From<Vec<pallas::Base>> for NoteCommitmentTree {
    /// Compute the tree from a whole bunch of note commitments at once.
    fn from(values: Vec<pallas::Base>) -> Self {
        let mut tree = Self::default();

        if values.is_empty() {
            return tree;
        }

        for cm_x in values {
            let _ = tree.append(cm_x);
        }

        tree
    }
}
