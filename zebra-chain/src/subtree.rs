//! Struct representing Sapling/Orchard note commitment subtrees

use std::sync::Arc;

use crate::block::Height;

/// Height at which Zebra tracks subtree roots
pub const TRACKED_SUBTREE_HEIGHT: u8 = 16;

/// A subtree index
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NoteCommitmentSubtreeIndex(pub u16);

impl From<u16> for NoteCommitmentSubtreeIndex {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

/// Subtree root of Sapling or Orchard note commitment tree,
/// with its associated block height and subtree index.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NoteCommitmentSubtree<Node> {
    /// Index of this subtree
    pub index: NoteCommitmentSubtreeIndex,
    /// End boundary of this subtree, the block height of its last leaf.
    pub end: Height,
    /// Root of this subtree.
    pub node: Node,
}

impl<Node> NoteCommitmentSubtree<Node> {
    /// Creates new [`NoteCommitmentSubtree`]
    pub fn new(index: impl Into<NoteCommitmentSubtreeIndex>, end: Height, node: Node) -> Arc<Self> {
        let index = index.into();
        Arc::new(Self { index, end, node })
    }
}

/// Subtree root of Sapling or Orchard note commitment tree, with block height, but without the subtree index.
/// Used for database key-value serialization, where the subtree index is the key, and this struct is the value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NoteCommitmentSubtreeData<Node> {
    /// End boundary of this subtree, the block height of its last leaf.
    pub end: Height,
    /// Root of this subtree.
    pub node: Node,
}

impl<Node> NoteCommitmentSubtreeData<Node> {
    /// Creates new [`NoteCommitmentSubtreeData`]
    pub fn new(end: Height, node: Node) -> Self {
        Self { end, node }
    }

    /// Creates new [`NoteCommitmentSubtree`] from a [`NoteCommitmentSubtreeData`] and index
    pub fn with_index(
        self,
        index: impl Into<NoteCommitmentSubtreeIndex>,
    ) -> Arc<NoteCommitmentSubtree<Node>> {
        NoteCommitmentSubtree::new(index, self.end, self.node)
    }
}
