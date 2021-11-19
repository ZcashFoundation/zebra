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

use std::{cell::Cell, fmt};

use byteorder::{BigEndian, ByteOrder};
use incrementalmerkletree::{bridgetree, Frontier};
use lazy_static::lazy_static;
use thiserror::Error;

use super::commitment::NoteCommitment;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use sha2::digest::generic_array::GenericArray;

/// Sprout note commitment trees have a max depth of 29.
///
/// <https://zips.z.cash/protocol/protocol.pdf#constants>
pub(super) const MERKLE_DEPTH: usize = 29;

/// [MerkleCRH^Sprout] Hash Function.
///
/// Creates nodes of the note commitment tree.
///
/// MerkleCRH^Sprout(layer, left, right) := SHA256Compress(left || right).
///
/// Note: the implementation of MerkleCRH^Sprout does not use the `layer`
/// argument from the definition above since the argument does not affect the output.
///
/// [MerkleCRH^Sprout]: https://zips.z.cash/protocol/protocol.pdf#merklecrh.
fn merkle_crh_sprout(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut other_block = [0u8; 64];
    other_block[..32].copy_from_slice(&left[..]);
    other_block[32..].copy_from_slice(&right[..]);

    // H256: SHA-256 initial state.
    // https://github.com/RustCrypto/hashes/blob/master/sha2/src/consts.rs#L170
    let mut state = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    sha2::compress256(&mut state, &[GenericArray::clone_from_slice(&other_block)]);

    // Yes, SHA-256 does big endian here.
    // https://github.com/RustCrypto/hashes/blob/master/sha2/src/sha256.rs#L40
    let mut derived_bytes = [0u8; 32];
    BigEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

lazy_static! {
    /// List of "empty" Sprout note commitment roots (nodes), one for each layer.
    ///
    /// The list is indexed by the layer number (0: root; `MERKLE_DEPTH`: leaf).
    pub(super) static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // The empty leaf node at layer `MERKLE_DEPTH`.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer `MERKLE_DEPTH` - 1 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for _ in 0..MERKLE_DEPTH {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            v.insert(0, merkle_crh_sprout(v[0], v[0]));
        }

        v
    };
}

/// The index of a note's commitment at the leafmost layer of its Note
/// Commitment Tree.
///
/// https://zips.z.cash/protocol/protocol.pdf#merkletree
pub struct Position(pub(crate) u64);

/// Sprout note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sprout note
/// commitment tree corresponding to the final Sprout treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Root([u8; 32]);

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
    fn from(rt: Root) -> [u8; 32] {
        rt.0
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

/// A node of the Sprout note commitment tree.
#[derive(Clone, Debug)]
struct Node([u8; 32]);

impl incrementalmerkletree::Hashable for Node {
    /// Returns an empty leaf.
    fn empty_leaf() -> Self {
        Self(NoteCommitmentTree::uncommitted())
    }

    /// Combines two nodes to generate a new node using [MerkleCRH^Sprout].
    ///
    /// Note that Sprout does not use the `level` argument.
    ///
    /// [MerkleCRH^Sprout]: https://zips.z.cash/protocol/protocol.pdf#sproutmerklecrh
    fn combine(_level: incrementalmerkletree::Altitude, a: &Self, b: &Self) -> Self {
        Self(merkle_crh_sprout(a.0, b.0))
    }

    /// Returns the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Altitude) -> Self {
        let layer_below: usize = MERKLE_DEPTH - usize::from(level);
        Self(EMPTY_ROOTS[layer_below])
    }
}

impl From<NoteCommitment> for Node {
    fn from(cm: NoteCommitment) -> Self {
        Node(cm.into())
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
        let cm = NoteCommitment::from(bytes);
        let node = Node::from(cm);

        Ok(node)
    }
}

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum NoteCommitmentTreeError {
    #[error("the note commitment tree is full")]
    FullTree,
}

/// [Sprout Note Commitment Tree].
///
/// An incremental Merkle tree of fixed depth used to store Sprout note commitments.
/// It is used to express the existence of value and the capability to spend it. It is _not_ the
/// job of this tree to protect against double-spending, as it is append-only; double-spending
/// is prevented by maintaining the [nullifier set] for each shielded pool.
///
/// Internally this wraps [`incrementalmerkletree::bridgetree::Frontier`], so that we can maintain and increment
/// the full tree with only the minimal amount of non-empty nodes/leaves required.
///
/// [Sprout Note Commitment Tree]: https://zips.z.cash/protocol/protocol.pdf#merkletree
/// [nullifier set]: https://zips.z.cash/protocol/protocol.pdf#nullifierset
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteCommitmentTree {
    /// The tree represented as a [`incrementalmerkletree::bridgetree::Frontier`].
    ///
    /// A [`incrementalmerkletree::Frontier`] is a subset of the tree that allows to fully specify it. It
    /// consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    inner: bridgetree::Frontier<Node, { MERKLE_DEPTH as u8 }>,

    /// A cached root of the tree.
    ///
    /// Every time the root is computed by [`Self::root`], it is cached here,
    /// and the cached value will be returned by [`Self::root`] until the tree
    /// is changed by [`Self::append`]. This greatly increases performance
    /// because it avoids recomputing the root when the tree does not change
    /// between blocks. In the finalized state, the tree is read from disk for
    /// every block processed, which would also require recomputing the root
    /// even if it has not changed (note that the cached root is serialized with
    /// the tree). This is particularly important since we decided to
    /// instantiate the trees from the genesis block, for simplicity.
    ///
    /// [`Cell`] offers interior mutability (it works even with a non-mutable
    /// reference to the tree) but it prevents the tree (and anything that uses
    /// it) from being shared between threads. If this ever becomes an issue we
    /// can leave caching to the callers (which requires much more code), or
    /// replace `Cell` with `Arc<Mutex<_>>` (and be careful of deadlocks and
    /// async code.)
    cached_root: Cell<Option<Root>>,
}

impl NoteCommitmentTree {
    /// Appends a note commitment to the leafmost layer of the tree.
    ///
    /// Returns an error if the tree is full.
    pub fn append(&mut self, cm: NoteCommitment) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(&cm.into()) {
            // Invalidate cached root
            self.cached_root.replace(None);
            Ok(())
        } else {
            Err(NoteCommitmentTreeError::FullTree)
        }
    }

    /// Returns the current root of the tree; used as an anchor in Sprout
    /// shielded transactions.
    pub fn root(&self) -> Root {
        match self.cached_root.get() {
            // Return cached root.
            Some(root) => root,
            None => {
                // Compute root and cache it.
                let root = Root(self.inner.root().0);
                self.cached_root.replace(Some(root));
                root
            }
        }
    }

    /// Returns a hash of the Sprout note commitment tree root.
    pub fn hash(&self) -> [u8; 32] {
        self.root().into()
    }

    /// Returns an as-yet unused leaf node value of a Sprout note commitment tree.
    ///
    /// Uncommitted^Sprout = [0]^(l^[Sprout_Merkle]).
    ///
    /// [Sprout_Merkle]: https://zips.z.cash/protocol/protocol.pdf#constants
    pub fn uncommitted() -> [u8; 32] {
        [0; 32]
    }

    /// Counts the note commitments in the tree.
    ///
    /// For Sprout, the tree is [capped at 2^29 leaf nodes][spec].
    ///
    /// [spec]: https://zips.z.cash/protocol/protocol.pdf#merkletree
    pub fn count(&self) -> u64 {
        self.inner.position().map_or(0, |pos| u64::from(pos) + 1)
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

impl From<Vec<NoteCommitment>> for NoteCommitmentTree {
    /// Builds the tree from a vector of commitments at once.
    fn from(values: Vec<NoteCommitment>) -> Self {
        let mut tree = Self::default();

        if values.is_empty() {
            return tree;
        }

        for cm in values {
            let _ = tree.append(cm);
        }

        tree
    }
}
