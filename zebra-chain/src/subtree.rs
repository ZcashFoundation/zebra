//! Struct representing Sapling/Orchard note commitment subtrees

use std::{fmt, num::TryFromIntError};

use serde::{Deserialize, Serialize};

use crate::block::Height;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// Height at which Zebra tracks subtree roots
pub const TRACKED_SUBTREE_HEIGHT: u8 = 16;

/// A note commitment subtree index, used to identify a subtree in a shielded pool.
/// Also used to count subtrees.
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[serde(transparent)]
pub struct NoteCommitmentSubtreeIndex(pub u16);

impl fmt::Display for NoteCommitmentSubtreeIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_string())
    }
}

impl From<u16> for NoteCommitmentSubtreeIndex {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl TryFrom<u64> for NoteCommitmentSubtreeIndex {
    type Error = TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        u16::try_from(value).map(Self)
    }
}

// If we want to automatically convert NoteCommitmentSubtreeIndex to the generic integer literal
// type, we can only implement conversion into u64. (Or u16, but not both.)
impl From<NoteCommitmentSubtreeIndex> for u64 {
    fn from(value: NoteCommitmentSubtreeIndex) -> Self {
        value.0.into()
    }
}

// TODO:
// - consider defining sapling::SubtreeRoot and orchard::SubtreeRoot types or type wrappers,
//   to avoid type confusion between the leaf Node and subtree root types.

/// Subtree root of Sapling or Orchard note commitment tree,
/// with its associated block height and subtree index.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct NoteCommitmentSubtree<SubtreeRoot> {
    /// Index of this subtree
    pub index: NoteCommitmentSubtreeIndex,
    /// Root of this subtree.
    pub root: SubtreeRoot,
    /// End boundary of this subtree, the block height of its last leaf.
    pub end_height: Height,
}

impl<SubtreeRoot> NoteCommitmentSubtree<SubtreeRoot> {
    /// Creates new [`NoteCommitmentSubtree`]
    pub fn new(
        index: impl Into<NoteCommitmentSubtreeIndex>,
        end_height: Height,
        root: SubtreeRoot,
    ) -> Self {
        let index = index.into();
        Self {
            index,
            end_height,
            root,
        }
    }

    /// Converts struct to [`NoteCommitmentSubtreeData`].
    pub fn into_data(self) -> NoteCommitmentSubtreeData<SubtreeRoot> {
        NoteCommitmentSubtreeData::new(self.end_height, self.root)
    }
}

/// Subtree root of Sapling or Orchard note commitment tree, with block height, but without the subtree index.
/// Used for database key-value serialization, where the subtree index is the key, and this struct is the value.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct NoteCommitmentSubtreeData<SubtreeRoot> {
    /// Merkle root of the 2^16-leaf subtree.
    pub root: SubtreeRoot,

    /// Height of the block containing the note that completed this subtree.
    pub end_height: Height,
}

impl<SubtreeRoot> NoteCommitmentSubtreeData<SubtreeRoot> {
    /// Creates new [`NoteCommitmentSubtreeData`]
    pub fn new(end_height: Height, root: SubtreeRoot) -> Self {
        Self { end_height, root }
    }

    /// Creates new [`NoteCommitmentSubtree`] from a [`NoteCommitmentSubtreeData`] and index
    pub fn with_index(
        self,
        index: impl Into<NoteCommitmentSubtreeIndex>,
    ) -> NoteCommitmentSubtree<SubtreeRoot> {
        NoteCommitmentSubtree::new(index, self.end_height, self.root)
    }
}
