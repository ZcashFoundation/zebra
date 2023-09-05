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
    fmt,
    hash::{Hash, Hasher},
    io,
    sync::Arc,
};

use bitvec::prelude::*;
use bridgetree::{self, NonEmptyFrontier};
use hex::ToHex;
use incrementalmerkletree::{frontier::Frontier, Hashable};

use lazy_static::lazy_static;
use thiserror::Error;
use zcash_encoding::{Optional, Vector};
use zcash_primitives::merkle_tree::HashSer;

use super::commitment::pedersen_hashes::pedersen_hash;

use crate::{
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
    subtree::TRACKED_SUBTREE_HEIGHT,
};

pub mod legacy;
use legacy::{LegacyLeaf, LegacyNoteCommitmentTree};

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
#[derive(Copy, Clone, Eq, PartialEq)]
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

/// Required to convert [`NoteCommitmentTree`] into [`SerializedTree`].
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

    /// Returns true if the most recently appended leaf completes the subtree
    pub fn is_complete_subtree(tree: &NonEmptyFrontier<Node>) -> bool {
        tree.position()
            .is_complete_subtree(TRACKED_SUBTREE_HEIGHT.into())
    }

    /// Returns subtree address at [`TRACKED_SUBTREE_HEIGHT`]
    pub fn subtree_address(tree: &NonEmptyFrontier<Node>) -> incrementalmerkletree::Address {
        incrementalmerkletree::Address::above_position(
            TRACKED_SUBTREE_HEIGHT.into(),
            tree.position(),
        )
    }

    /// Returns subtree index and root if the most recently appended leaf completes the subtree
    #[allow(clippy::unwrap_in_result)]
    pub fn completed_subtree_index_and_root(&self) -> Option<(u16, Node)> {
        let value = self.inner.value()?;
        Self::is_complete_subtree(value).then_some(())?;
        let address = Self::subtree_address(value);
        let index = address.index().try_into().expect("should fit in u16");
        let root = value.root(Some(TRACKED_SUBTREE_HEIGHT.into()));

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
        assert_eq!(SerializedTree::from(self), SerializedTree::from(other));
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
            inner: bridgetree::Frontier::empty(),
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

/// A serialized Sapling note commitment tree.
///
/// The format of the serialized data is compatible with
/// [`CommitmentTree`](incrementalmerkletree::frontier::CommitmentTree) from `librustzcash` and not
/// with [`Frontier`] from the crate
/// [`incrementalmerkletree`]. Zebra follows the former format in order to stay
/// consistent with `zcashd` in RPCs. Note that [`NoteCommitmentTree`] itself is
/// represented as [`Frontier`].
///
/// The formats are semantically equivalent. The primary difference between them
/// is that in [`Frontier`], the vector of parents is
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
/// compatible with [`Frontier`] instead.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SerializedTree(Vec<u8>);

impl From<&NoteCommitmentTree> for SerializedTree {
    fn from(tree: &NoteCommitmentTree) -> Self {
        let mut serialized_tree = vec![];

        //
        let legacy_tree = LegacyNoteCommitmentTree::from(tree.clone());

        // Convert the note commitment tree represented as a frontier into the
        // format compatible with `zcashd`.
        //
        // `librustzcash` has a function [`from_frontier()`][1], which returns a
        // commitment tree in the sparse format. However, the returned tree
        // always contains [`MERKLE_DEPTH`] parent nodes, even though some
        // trailing parents are empty. Such trees are incompatible with Sapling
        // commitment trees returned by `zcashd` because `zcashd` returns
        // Sapling commitment trees without empty trailing parents. For this
        // reason, Zebra implements its own conversion between the dense and
        // sparse formats for Sapling.
        //
        // [1]: <https://github.com/zcash/librustzcash/blob/a63a37a/zcash_primitives/src/merkle_tree.rs#L125>
        if let Some(frontier) = legacy_tree.inner.frontier {
            let (left_leaf, right_leaf) = match frontier.leaf {
                LegacyLeaf::Left(left_value) => (Some(left_value), None),
                LegacyLeaf::Right(left_value, right_value) => (Some(left_value), Some(right_value)),
            };

            // Ommers are siblings of parent nodes along the branch from the
            // most recent leaf to the root of the tree.
            let mut ommers_iter = frontier.ommers.iter();

            // Set bits in the binary representation of the position indicate
            // the presence of ommers along the branch from the most recent leaf
            // node to the root of the tree, except for the lowest bit.
            let mut position: u64 = (frontier.position.0)
                .try_into()
                .expect("old usize position always fit in u64");

            // The lowest bit does not indicate the presence of any ommers. We
            // clear it so that we can test if there are no set bits left in
            // [`position`].
            position &= !1;

            // Run through the bits of [`position`], and push an ommer for each
            // set bit, or `None` otherwise. In contrast to the 'zcashd' code
            // linked above, we want to skip any trailing `None` parents at the
            // top of the tree. To do that, we clear the bits as we go through
            // them, and break early if the remaining bits are all zero (i.e.
            // [`position`] is zero).
            let mut parents = vec![];
            for i in 1..MERKLE_DEPTH {
                // Test each bit in [`position`] individually. Don't test the
                // lowest bit since it doesn't actually indicate the position of
                // any ommer.
                let bit_mask = 1 << i;

                if position & bit_mask == 0 {
                    parents.push(None);
                } else {
                    parents.push(ommers_iter.next());
                    // Clear the set bit so that we can test if there are no set
                    // bits left.
                    position &= !bit_mask;
                    // If there are no set bits left, exit early so that there
                    // are no empty trailing parent nodes in the serialized
                    // tree.
                    if position == 0 {
                        break;
                    }
                }
            }

            // Serialize the converted note commitment tree.
            Optional::write(&mut serialized_tree, left_leaf, |tree, leaf| {
                leaf.write(tree)
            })
            .expect("A leaf in a note commitment tree should be serializable");

            Optional::write(&mut serialized_tree, right_leaf, |tree, leaf| {
                leaf.write(tree)
            })
            .expect("A leaf in a note commitment tree should be serializable");

            Vector::write(&mut serialized_tree, &parents, |tree, parent| {
                Optional::write(tree, *parent, |tree, parent| parent.write(tree))
            })
            .expect("Parent nodes in a note commitment tree should be serializable");
        }

        Self(serialized_tree)
    }
}

impl From<Option<Arc<NoteCommitmentTree>>> for SerializedTree {
    fn from(maybe_tree: Option<Arc<NoteCommitmentTree>>) -> Self {
        match maybe_tree {
            Some(tree) => tree.as_ref().into(),
            None => Self(vec![]),
        }
    }
}

impl AsRef<[u8]> for SerializedTree {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
