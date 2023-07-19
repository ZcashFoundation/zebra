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

use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
    sync::Arc,
};

use bitvec::prelude::*;
use bridgetree;
use halo2::pasta::{group::ff::PrimeField, pallas};
use incrementalmerkletree::Hashable;
use lazy_static::lazy_static;
use thiserror::Error;
use zcash_primitives::merkle_tree::{write_commitment_tree, HashSer};

use super::sinsemilla::*;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

pub mod legacy;
use legacy::LegacyNoteCommitmentTree;

/// The type that is used to update the note commitment tree.
///
/// Unfortunately, this is not the same as `orchard::NoteCommitment`.
pub type NoteCommitmentUpdate = pallas::Base;

pub(super) const MERKLE_DEPTH: u8 = 32;

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
/// <https://zips.z.cash/protocol/protocol.pdf#orchardmerklecrh>
/// <https://zips.z.cash/protocol/protocol.pdf#constants>
fn merkle_crh_orchard(layer: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let mut s = bitvec![u8, Lsb0;];

    // Prefix: l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer)
    let l = MERKLE_DEPTH - 1 - layer;
    s.extend_from_bitslice(&BitArray::<_, Lsb0>::from([l, 0])[0..10]);
    s.extend_from_bitslice(&BitArray::<_, Lsb0>::from(left.to_repr())[0..255]);
    s.extend_from_bitslice(&BitArray::<_, Lsb0>::from(right.to_repr())[0..255]);

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
    /// <https://zips.z.cash/protocol/protocol.pdf#constants>
    pub(super) static ref EMPTY_ROOTS: Vec<pallas::Base> = {
        // The empty leaf node. This is layer 32.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for layer in (0..MERKLE_DEPTH).rev()
        {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            let next = merkle_crh_orchard(layer, v[0], v[0]);
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
#[derive(Clone, Copy, Default, Eq, Serialize, Deserialize)]
pub struct Root(#[serde(with = "serde_helpers::Base")] pub(crate) pallas::Base);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root")
            .field(&hex::encode(self.0.to_repr()))
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

impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        // TODO: should we compare canonical forms here using `.to_repr()`?
        self.0 == other.0
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
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Node(pallas::Base);

/// Required to convert [`NoteCommitmentTree`] into [`SerializedTree`].
///
/// Zebra stores Orchard note commitment trees as [`Frontier`][1]s while the
/// [`z_gettreestate`][2] RPC requires [`CommitmentTree`][3]s. Implementing
/// [`HashSer`] for [`Node`]s allows the conversion.
///
/// [1]: bridgetree::Frontier
/// [2]: https://zcash.github.io/rpc/z_gettreestate.html
/// [3]: incrementalmerkletree::frontier::CommitmentTree
impl HashSer for Node {
    fn read<R: io::Read>(mut reader: R) -> io::Result<Self> {
        let mut repr = [0u8; 32];
        reader.read_exact(&mut repr)?;
        let maybe_node = pallas::Base::from_repr(repr).map(Self);

        <Option<_>>::from(maybe_node).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Non-canonical encoding of Pallas base field value.",
            )
        })
    }

    fn write<W: io::Write>(&self, mut writer: W) -> io::Result<()> {
        writer.write_all(&self.0.to_repr())
    }
}

impl Hashable for Node {
    fn empty_leaf() -> Self {
        Self(NoteCommitmentTree::uncommitted())
    }

    /// Combine two nodes to generate a new node in the given level.
    /// Level 0 is the layer above the leaves (layer 31).
    /// Level 31 is the root (layer 0).
    fn combine(level: incrementalmerkletree::Level, a: &Self, b: &Self) -> Self {
        let layer = MERKLE_DEPTH - 1 - u8::from(level);
        Self(merkle_crh_orchard(layer, a.0, b.0))
    }

    /// Return the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Level) -> Self {
        let layer_below = usize::from(MERKLE_DEPTH) - usize::from(level);
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

#[derive(Error, Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
pub enum NoteCommitmentTreeError {
    #[error("The note commitment tree is full")]
    FullTree,
}

/// Orchard Incremental Note Commitment Tree
#[derive(Debug, Serialize, Deserialize)]
#[serde(into = "LegacyNoteCommitmentTree")]
#[serde(from = "LegacyNoteCommitmentTree")]
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
    inner: bridgetree::Frontier<Node, MERKLE_DEPTH>,

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
    /// We use a [`RwLock`](std::sync::RwLock) for this cache, because it is
    /// only written once per tree update. Each tree has its own cached root, a
    /// new lock is created for each clone.
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
    #[allow(clippy::unwrap_in_result)]
    pub fn append(&mut self, cm_x: NoteCommitmentUpdate) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(cm_x.into()) {
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
        if let Some(root) = self.cached_root() {
            // Return cached root.
            return root;
        }

        // Get exclusive access, compute the root, and cache it.
        let mut write_root = self
            .cached_root
            .write()
            .expect("a thread that previously held exclusive lock access panicked");
        let read_root = write_root.as_ref().cloned();
        match read_root {
            // Another thread got write access first, return cached root.
            Some(root) => root,
            None => {
                // Compute root and cache it.
                let root = self.recalculate_root();
                *write_root = Some(root);
                root
            }
        }
    }

    /// Returns the current root of the tree, if it has already been cached.
    #[allow(clippy::unwrap_in_result)]
    pub fn cached_root(&self) -> Option<Root> {
        *self
            .cached_root
            .read()
            .expect("a thread that previously held exclusive lock access panicked")
    }

    /// Calculates and returns the current root of the tree, ignoring any caching.
    pub fn recalculate_root(&self) -> Root {
        Root(self.inner.root().0)
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
        self.inner
            .value()
            .map_or(0, |x| u64::from(x.position()) + 1)
    }

    /// Checks if the tree roots and inner data structures of `self` and `other` are equal.
    ///
    /// # Panics
    ///
    /// If they aren't equal, with a message explaining the differences.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn assert_frontier_eq(&self, other: &Self) {
        // It's technically ok for the cached root not to be preserved,
        // but it can result in expensive cryptographic operations,
        // so we fail the tests if it happens.
        assert_eq!(self.cached_root(), other.cached_root());

        // Check the data in the internal data structure
        assert_eq!(self.inner, other.inner);

        // Check the RPC serialization format (not the same as the Zebra database format)
        assert_eq!(SerializedTree::from(self), SerializedTree::from(other));
    }
}

impl Clone for NoteCommitmentTree {
    /// Clones the inner tree, and creates a new `RwLock` with the cloned root data.
    fn clone(&self) -> Self {
        let cached_root = self.cached_root();

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

/// A serialized Orchard note commitment tree.
///
/// The format of the serialized data is compatible with
/// [`CommitmentTree`](incrementalmerkletree::frontier::CommitmentTree) from `librustzcash` and not
/// with [`Frontier`](bridgetree::Frontier) from the crate
/// [`incrementalmerkletree`]. Zebra follows the former format in order to stay
/// consistent with `zcashd` in RPCs. Note that [`NoteCommitmentTree`] itself is
/// represented as [`Frontier`](bridgetree::Frontier).
///
/// The formats are semantically equivalent. The primary difference between them
/// is that in [`Frontier`](bridgetree::Frontier), the vector of parents is
/// dense (we know where the gaps are from the position of the leaf in the
/// overall tree); whereas in [`CommitmentTree`](incrementalmerkletree::frontier::CommitmentTree),
/// the vector of parent hashes is sparse with [`None`] values in the gaps.
///
/// The sparse format, used in this implementation, allows representing invalid
/// commitment trees while the dense format allows representing only valid
/// commitment trees.
///
/// It is likely that the dense format will be used in future RPCs, in which
/// case the current implementation will have to change and use the format
/// compatible with [`Frontier`](bridgetree::Frontier) instead.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SerializedTree(Vec<u8>);

impl From<&NoteCommitmentTree> for SerializedTree {
    fn from(tree: &NoteCommitmentTree) -> Self {
        let mut serialized_tree = vec![];

        // Skip the serialization of empty trees.
        //
        // Note: This ensures compatibility with `zcashd` in the
        // [`z_gettreestate`][1] RPC.
        //
        // [1]: https://zcash.github.io/rpc/z_gettreestate.html
        if tree.inner == bridgetree::Frontier::empty() {
            return Self(serialized_tree);
        }

        // Convert the note commitment tree from
        // [`Frontier`](bridgetree::Frontier) to
        // [`CommitmentTree`](merkle_tree::CommitmentTree).
        let tree = incrementalmerkletree::frontier::CommitmentTree::from_frontier(&tree.inner);

        write_commitment_tree(&tree, &mut serialized_tree)
            .expect("note commitment tree should be serializable");
        Self(serialized_tree)
    }
}

impl From<Option<Arc<NoteCommitmentTree>>> for SerializedTree {
    fn from(maybe_tree: Option<Arc<NoteCommitmentTree>>) -> Self {
        match maybe_tree {
            Some(tree) => tree.as_ref().into(),
            None => Self(Vec::new()),
        }
    }
}

impl AsRef<[u8]> for SerializedTree {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
