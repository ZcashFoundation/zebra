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

use std::fmt;

use byteorder::{BigEndian, ByteOrder};
use incrementalmerkletree::frontier::Frontier;
use lazy_static::lazy_static;
use sha2::digest::generic_array::GenericArray;
use thiserror::Error;

use super::commitment::NoteCommitment;

use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Sprout note commitment trees have a max depth of 29.
///
/// <https://zips.z.cash/protocol/protocol.pdf#constants>
pub(super) const MERKLE_DEPTH: u8 = 29;

/// [MerkleCRH^Sprout] Hash Function.
///
/// Creates nodes of the note commitment tree.
///
/// MerkleCRH^Sprout(layer, left, right) := SHA256Compress(left || right).
///
/// Note: the implementation of MerkleCRH^Sprout does not use the `layer`
/// argument from the definition above since the argument does not affect the output.
///
/// [MerkleCRH^Sprout]: https://zips.z.cash/protocol/protocol.pdf#merklecrh
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
/// <https://zips.z.cash/protocol/protocol.pdf#merkletree>
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
        f.debug_tuple("Root").field(&hex::encode(self.0)).finish()
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

impl ZcashSerialize for Root {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&<[u8; 32]>::from(*self)[..])?;

        Ok(())
    }
}

impl ZcashDeserialize for Root {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self::from(reader.read_32_bytes()?))
    }
}

/// A node of the Sprout note commitment tree.
#[derive(Clone, Copy, Eq, PartialEq)]
struct Node([u8; 32]);

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Node").field(&hex::encode(self.0)).finish()
    }
}

/// Required to convert [`NoteCommitmentTree`] into [`SerializedTree`].
///
/// Zebra stores Sapling note commitment trees as [`Frontier`][1]s while the
/// [`z_gettreestate`][2] RPC requires [`CommitmentTree`][3]s. Implementing
/// [`incrementalmerkletree::Hashable`] for [`Node`]s allows the conversion.
///
/// [1]: bridgetree::Frontier
/// [2]: https://zcash.github.io/rpc/z_gettreestate.html
/// [3]: incrementalmerkletree::frontier::CommitmentTree
impl zcash_primitives::merkle_tree::HashSer for Node {
    fn read<R: std::io::Read>(mut reader: R) -> std::io::Result<Self> {
        let mut node = [0u8; 32];
        reader.read_exact(&mut node)?;
        Ok(Self(node))
    }

    fn write<W: std::io::Write>(&self, mut writer: W) -> std::io::Result<()> {
        writer.write_all(self.0.as_ref())
    }
}

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
    fn combine(_level: incrementalmerkletree::Level, a: &Self, b: &Self) -> Self {
        Self(merkle_crh_sprout(a.0, b.0))
    }

    /// Returns the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Level) -> Self {
        let layer_below = usize::from(MERKLE_DEPTH) - usize::from(level);
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

#[derive(Error, Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
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
/// Internally this wraps [`bridgetree::Frontier`], so that we can maintain and increment
/// the full tree with only the minimal amount of non-empty nodes/leaves required.
///
/// [Sprout Note Commitment Tree]: https://zips.z.cash/protocol/protocol.pdf#merkletree
/// [nullifier set]: https://zips.z.cash/protocol/protocol.pdf#nullifierset
#[derive(Debug)]
pub struct NoteCommitmentTree {
    /// The tree represented as a [`bridgetree::Frontier`].
    ///
    /// A [`bridgetree::Frontier`] is a subset of the tree that allows to fully specify it. It
    /// consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    ///
    /// # Consensus
    ///
    /// > A block MUST NOT add Sprout note commitments that would result in the Sprout note commitment tree
    /// > exceeding its capacity of 2^(MerkleDepth^Sprout) leaf nodes.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#merkletree>
    ///
    /// Note: MerkleDepth^Sprout = MERKLE_DEPTH = 29.
    inner: Frontier<Node, MERKLE_DEPTH>,

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
    /// We use a [`RwLock`](std::sync::RwLock) for this cache, because it is
    /// only written once per tree update. Each tree has its own cached root, a
    /// new lock is created for each clone.
    cached_root: std::sync::RwLock<Option<Root>>,
}

impl serde::Serialize for NoteCommitmentTree {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(&self.as_bytes())
    }
}

impl NoteCommitmentTree {
    /// Appends a note commitment to the leafmost layer of the tree.
    ///
    /// Returns an error if the tree is full.
    #[allow(clippy::unwrap_in_result)]
    pub fn append(&mut self, cm: NoteCommitment) -> Result<(), NoteCommitmentTreeError> {
        if self.inner.append(cm.into()) {
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

    /// Returns the current root of the tree; used as an anchor in Sprout
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
                let root = Root(self.inner.root().0);
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

    /// Returns a hash of the Sprout note commitment tree root.
    pub fn hash(&self) -> [u8; 32] {
        self.root().into()
    }

    /// Returns an as-yet unused leaf node value of a Sprout note commitment tree.
    ///
    /// Uncommitted^Sprout = \[0\]^(l^[Sprout_Merkle]).
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
    }

    /// Get raw bytes given a [`CommitmentTree`].
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        if let Some(frontier_data) = self.inner.value() {
            let ommers: Vec<Node> = frontier_data.ommers().to_vec();
            let leaf: Node = *frontier_data.leaf();

            let frontier32: Frontier<Node, 32> =
                Frontier::from_parts(frontier_data.position(), leaf, ommers)
                    .unwrap_or(Frontier::empty());

            zcash_primitives::merkle_tree::write_frontier_v1(&mut buf, &frontier32)
                .expect("frontier writting should not fail because we got all the data needed");

            if let Some(cached_root) = self.cached_root() {
                buf.append(
                    &mut Root::zcash_serialize_to_vec(&cached_root)
                        .expect("if we have a root then it should be serialaizable"),
                );
            };
        }

        buf
    }

    /// Get a [`CommitmentTree`] from the raw bytes.
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let buf: Vec<u8> = bytes.as_ref().to_vec();
        let mut cursor = std::io::Cursor::new(buf);

        if let Ok(frontier_data) = zcash_primitives::merkle_tree::read_frontier_v1(&mut cursor) {
            let mut frontier29: Frontier<Node, 29> = Frontier::empty();
            if let Some(frontier_data_parts) = frontier_data.value() {
                let ommers: Vec<Node> = frontier_data_parts.ommers().to_vec();
                let leaf: Node = *frontier_data_parts.leaf();
                frontier29 = Frontier::from_parts(frontier_data_parts.position(), leaf, ommers)
                    .expect("no sprout note commitment tree should be bigger than 29 in depth");
            }

            let cached_root = match Root::zcash_deserialize(&mut cursor) {
                Ok(root) => std::sync::RwLock::new(Some(root)),
                _ => Default::default(),
            };

            NoteCommitmentTree {
                inner: frontier29,
                cached_root,
            }
        } else {
            NoteCommitmentTree::default()
        }
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
            inner: Frontier::empty(),
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
