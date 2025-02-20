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

use std::{
    default::Default,
    fmt,
    hash::{Hash, Hasher},
    io,
};

use bitvec::prelude::*;
use hex::ToHex;
use incrementalmerkletree::{
    frontier::{Frontier, NonEmptyFrontier},
    Hashable,
};

use lazy_static::lazy_static;
use thiserror::Error;
use zcash_primitives::merkle_tree::HashSer;

use super::commitment::pedersen_hashes::pedersen_hash;

use crate::{
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
    subtree::{NoteCommitmentSubtreeIndex, TRACKED_SUBTREE_HEIGHT},
};

pub mod legacy;
use legacy::LegacyNoteCommitmentTree;

/// The type that is used to update the note commitment tree.
///
/// Unfortunately, this is not the same as `sapling::NoteCommitment`.
pub type NoteCommitmentUpdate = jubjub::Fq;

pub(super) const MERKLE_DEPTH: u8 = 32;

/// MerkleCRH^Sapling Hash Function
///
/// Used to hash incremental Merkle tree hash values for Sapling.
///
/// MerkleCRH^Sapling(layer, left, right) := PedersenHash("Zcash_PH", l || left || right)
/// where l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer) and
/// left, right, and the output are all technically 255 bits (l_MerkleSapling), not 256.
///
/// <https://zips.z.cash/protocol/protocol.pdf#merklecrh>
fn merkle_crh_sapling(layer: u8, left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut s = bitvec![u8, Lsb0;];

    // Prefix: l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
    let l = MERKLE_DEPTH - 1 - layer;
    s.extend_from_bitslice(&BitSlice::<_, Lsb0>::from_element(&l)[0..6]);
    s.extend_from_bitslice(&BitArray::<_, Lsb0>::from(left)[0..255]);
    s.extend_from_bitslice(&BitArray::<_, Lsb0>::from(right)[0..255]);

    pedersen_hash(*b"Zcash_PH", &s).to_bytes()
}

lazy_static! {
    /// List of "empty" Sapling note commitment nodes, one for each layer.
    ///
    /// The list is indexed by the layer number (0: root; MERKLE_DEPTH: leaf).
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#constants>
    pub(super) static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // The empty leaf node. This is layer 32.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for layer in (0..MERKLE_DEPTH).rev() {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            let next = merkle_crh_sapling(layer, v[0], v[0]);
            v.insert(0, next);
        }

        v

    };
}

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
            .field(&hex::encode(self.0.to_bytes()))
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

impl ToHex for &Root {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        <[u8; 32]>::from(*self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        <[u8; 32]>::from(*self).encode_hex_upper()
    }
}

impl ToHex for Root {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
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
#[derive(Copy, Clone, Eq, PartialEq, Default)]
pub struct Node([u8; 32]);

impl AsRef<[u8; 32]> for Node {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("sapling::Node")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl Node {
    /// Return the node bytes in little-endian byte order suitable for printing out byte by byte.
    ///
    /// `zcashd`'s `z_getsubtreesbyindex` does not reverse the byte order of subtree roots.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        self.0
    }
}

impl ToHex for &Node {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for Node {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

/// Required to serialize [`NoteCommitmentTree`]s in a format matching `zcashd`.
///
/// Zebra stores Sapling note commitment trees as [`Frontier`]s while the
/// [`z_gettreestate`][1] RPC requires [`CommitmentTree`][2]s. Implementing
/// [`incrementalmerkletree::Hashable`] for [`Node`]s allows the conversion.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
/// [2]: incrementalmerkletree::frontier::CommitmentTree
impl HashSer for Node {
    fn read<R: io::Read>(mut reader: R) -> io::Result<Self> {
        let mut node = [0u8; 32];
        reader.read_exact(&mut node)?;
        Ok(Self(node))
    }

    fn write<W: io::Write>(&self, mut writer: W) -> io::Result<()> {
        writer.write_all(self.0.as_ref())
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
        Self(merkle_crh_sapling(layer, a.0, b.0))
    }

    /// Return the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Level) -> Self {
        let layer_below = usize::from(MERKLE_DEPTH) - usize::from(level);
        Self(EMPTY_ROOTS[layer_below])
    }
}

impl From<jubjub::Fq> for Node {
    fn from(x: jubjub::Fq) -> Self {
        Node(x.into())
    }
}

impl TryFrom<&[u8]> for Node {
    type Error = &'static str;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Option::<jubjub::Fq>::from(jubjub::Fq::from_bytes(
            bytes.try_into().map_err(|_| "wrong byte slice len")?,
        ))
        .map(Node::from)
        .ok_or("invalid jubjub field element")
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

#[derive(Error, Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
pub enum NoteCommitmentTreeError {
    #[error("The note commitment tree is full")]
    FullTree,
}

/// Sapling Incremental Note Commitment Tree.
///
/// Note that the default value of the [`Root`] type is `[0, 0, 0, 0]`. However, this value differs
/// from the default value of the root of the default tree which is the hash of the root's child
/// nodes. The default tree is the empty tree which has all leaves empty.
#[derive(Debug, Serialize, Deserialize)]
#[serde(into = "LegacyNoteCommitmentTree")]
#[serde(from = "LegacyNoteCommitmentTree")]
pub struct NoteCommitmentTree {
    /// The tree represented as a [`Frontier`].
    ///
    /// A Frontier is a subset of the tree that allows to fully specify it.
    /// It consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    ///
    /// # Consensus
    ///
    /// > [Sapling onward] A block MUST NOT add Sapling note commitments that
    /// > would result in the Sapling note commitment tree exceeding its capacity
    /// > of 2^(MerkleDepth^Sapling) leaf nodes.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#merkletree>
    ///
    /// Note: MerkleDepth^Sapling = MERKLE_DEPTH = 32.
    inner: Frontier<Node, MERKLE_DEPTH>,

    /// A cached root of the tree.
    ///
    /// Every time the root is computed by [`Self::root`] it is cached here, and
    /// the cached value will be returned by [`Self::root`] until the tree is
    /// changed by [`Self::append`]. This greatly increases performance because
    /// it avoids recomputing the root when the tree does not change between
    /// blocks. In the finalized state, the tree is read from disk for every
    /// block processed, which would also require recomputing the root even if
    /// it has not changed (note that the cached root is serialized with the
    /// tree). This is particularly important since we decided to instantiate
    /// the trees from the genesis block, for simplicity.
    ///
    /// We use a [`RwLock`](std::sync::RwLock) for this cache, because it is only written once per
    /// tree update. Each tree has its own cached root, a new lock is created
    /// for each clone.
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
    #[allow(clippy::unwrap_in_result)]
    pub fn append(&mut self, cm_u: NoteCommitmentUpdate) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(cm_u.into()) {
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

    /// Returns frontier of non-empty tree, or None.
    fn frontier(&self) -> Option<&NonEmptyFrontier<Node>> {
        self.inner.value()
    }

    /// Returns the position of the most recently appended leaf in the tree.
    ///
    /// This method is used for debugging, use `incrementalmerkletree::Address` for tree operations.
    pub fn position(&self) -> Option<u64> {
        let Some(tree) = self.frontier() else {
            // An empty tree doesn't have a previous leaf.
            return None;
        };

        Some(tree.position().into())
    }

    /// Returns true if this tree has at least one new subtree, when compared with `prev_tree`.
    pub fn contains_new_subtree(&self, prev_tree: &Self) -> bool {
        // Use -1 for the index of the subtree with no notes, so the comparisons are valid.
        let index = self.subtree_index().map_or(-1, |index| i32::from(index.0));
        let prev_index = prev_tree
            .subtree_index()
            .map_or(-1, |index| i32::from(index.0));

        // This calculation can't overflow, because we're using i32 for u16 values.
        let index_difference = index - prev_index;

        // There are 4 cases we need to handle:
        // - lower index: never a new subtree
        // - equal index: sometimes a new subtree
        // - next index: sometimes a new subtree
        // - greater than the next index: always a new subtree
        //
        // To simplify the function, we deal with the simple cases first.

        // There can't be any new subtrees if the current index is strictly lower.
        if index < prev_index {
            return false;
        }

        // There is at least one new subtree, even if there is a spurious index difference.
        if index_difference > 1 {
            return true;
        }

        // If the indexes are equal, there can only be a new subtree if `self` just completed it.
        if index == prev_index {
            return self.is_complete_subtree();
        }

        // If `self` is the next index, check if the last note completed a subtree.
        if self.is_complete_subtree() {
            return true;
        }

        // Then check for spurious index differences.
        //
        // There is one new subtree somewhere in the trees. It is either:
        // - a new subtree at the end of the previous tree, or
        // - a new subtree in this tree (but not at the end).
        //
        // Spurious index differences happen because the subtree index only increases when the
        // first note is added to the new subtree. So we need to exclude subtrees completed by the
        // last note commitment in the previous tree.
        //
        // We also need to exclude empty previous subtrees, because the index changes to zero when
        // the first note is added, but a subtree wasn't completed.
        if prev_tree.is_complete_subtree() || prev_index == -1 {
            return false;
        }

        // A new subtree was completed by a note commitment that isn't in the previous tree.
        true
    }

    /// Returns true if the most recently appended leaf completes the subtree
    pub fn is_complete_subtree(&self) -> bool {
        let Some(tree) = self.frontier() else {
            // An empty tree can't be a complete subtree.
            return false;
        };

        tree.position()
            .is_complete_subtree(TRACKED_SUBTREE_HEIGHT.into())
    }

    /// Returns the subtree index at [`TRACKED_SUBTREE_HEIGHT`].
    /// This is the number of complete or incomplete subtrees that are currently in the tree.
    /// Returns `None` if the tree is empty.
    #[allow(clippy::unwrap_in_result)]
    pub fn subtree_index(&self) -> Option<NoteCommitmentSubtreeIndex> {
        let tree = self.frontier()?;

        let index = incrementalmerkletree::Address::above_position(
            TRACKED_SUBTREE_HEIGHT.into(),
            tree.position(),
        )
        .index()
        .try_into()
        .expect("fits in u16");

        Some(index)
    }

    /// Returns the number of leaf nodes required to complete the subtree at
    /// [`TRACKED_SUBTREE_HEIGHT`].
    ///
    /// Returns `2^TRACKED_SUBTREE_HEIGHT` if the tree is empty.
    #[allow(clippy::unwrap_in_result)]
    pub fn remaining_subtree_leaf_nodes(&self) -> usize {
        let remaining = match self.frontier() {
            // If the subtree has at least one leaf node, the remaining number of nodes can be
            // calculated using the maximum subtree position and the current position.
            Some(tree) => {
                let max_position = incrementalmerkletree::Address::above_position(
                    TRACKED_SUBTREE_HEIGHT.into(),
                    tree.position(),
                )
                .max_position();

                max_position - tree.position().into()
            }
            // If the subtree has no nodes, the remaining number of nodes is the number of nodes in
            // a subtree.
            None => {
                let subtree_address = incrementalmerkletree::Address::above_position(
                    TRACKED_SUBTREE_HEIGHT.into(),
                    // This position is guaranteed to be in the first subtree.
                    0.into(),
                );

                assert_eq!(
                    subtree_address.position_range_start(),
                    0.into(),
                    "address is not in the first subtree"
                );

                subtree_address.position_range_end()
            }
        };

        u64::from(remaining).try_into().expect("fits in usize")
    }

    /// Returns subtree index and root if the most recently appended leaf completes the subtree
    pub fn completed_subtree_index_and_root(&self) -> Option<(NoteCommitmentSubtreeIndex, Node)> {
        if !self.is_complete_subtree() {
            return None;
        }

        let index = self.subtree_index()?;
        let root = self.frontier()?.root(Some(TRACKED_SUBTREE_HEIGHT.into()));

        Some((index, root))
    }

    /// Returns the current root of the tree, used as an anchor in Sapling
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
        Root::try_from(self.inner.root().0).unwrap()
    }

    /// Gets the Jubjub-based Pedersen hash of root node of this merkle tree of
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

    /// Counts of note commitments added to the tree.
    ///
    /// For Sapling, the tree is capped at 2^32.
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
        assert_eq!(self.to_rpc_bytes(), other.to_rpc_bytes());
    }

    /// Serializes [`Self`] to a format matching `zcashd`'s RPCs.
    pub fn to_rpc_bytes(&self) -> Vec<u8> {
        // Convert the tree from [`Frontier`](incrementalmerkletree::frontier::Frontier) to
        // [`CommitmentTree`](merkle_tree::CommitmentTree).
        let tree = incrementalmerkletree::frontier::CommitmentTree::from_frontier(&self.inner);

        let mut rpc_bytes = vec![];

        zcash_primitives::merkle_tree::write_commitment_tree(&tree, &mut rpc_bytes)
            .expect("serializable tree");

        rpc_bytes
    }
}

impl Clone for NoteCommitmentTree {
    /// Clones the inner tree, and creates a new [`RwLock`](std::sync::RwLock)
    /// with the cloned root data.
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
            inner: incrementalmerkletree::frontier::Frontier::empty(),
            cached_root: Default::default(),
        }
    }
}

impl Eq for NoteCommitmentTree {}

impl PartialEq for NoteCommitmentTree {
    fn eq(&self, other: &Self) -> bool {
        if let (Some(root), Some(other_root)) = (self.cached_root(), other.cached_root()) {
            // Use cached roots if available
            root == other_root
        } else {
            // Avoid expensive root recalculations which use multiple cryptographic hashes
            self.inner == other.inner
        }
    }
}

impl From<Vec<jubjub::Fq>> for NoteCommitmentTree {
    /// Computes the tree from a whole bunch of note commitments at once.
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
