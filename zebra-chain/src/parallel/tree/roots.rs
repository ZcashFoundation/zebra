//! Note commitment tree roots after a block.

use std::{io, sync::LazyLock};

use crate::{
    orchard,
    primitives::zcash_history::BlockCommitmentTreeRoots,
    sapling,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// The maximum number of blocks whose tree roots one request can ask for.
///
/// A response of this many roots is 384,000 bytes, well under the protocol's message size limit.
pub const MAX_TREE_ROOTS_PER_REQUEST: u32 = 4_000;

/// The Sapling, Orchard, and Ironwood note commitment tree roots after a block.
///
/// Before a pool activates, its root is the root of the empty tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TreeRoots {
    /// The Sapling note commitment tree root.
    pub sapling: sapling::tree::Root,
    /// The Orchard note commitment tree root.
    pub orchard: orchard::tree::Root,
    /// The Ironwood note commitment tree root.
    pub ironwood: orchard::tree::Root,
}

/// The roots of the empty trees, which take 96 hashes to compute.
static EMPTY_TREE_ROOTS: LazyLock<TreeRoots> = LazyLock::new(|| TreeRoots {
    sapling: sapling::tree::NoteCommitmentTree::default().root(),
    orchard: orchard::tree::NoteCommitmentTree::default().root(),
    ironwood: orchard::tree::NoteCommitmentTree::default().root(),
});

impl TreeRoots {
    /// The size of a serialized [`TreeRoots`].
    pub const SERIALIZED_SIZE: usize = 96;

    /// Returns the roots of the empty Sapling, Orchard, and Ironwood trees.
    pub fn empty() -> Self {
        *EMPTY_TREE_ROOTS
    }

    /// Returns these roots in the form the history tree takes them.
    pub fn as_block_commitment_tree_roots(&self) -> BlockCommitmentTreeRoots<'_> {
        BlockCommitmentTreeRoots {
            sapling: &self.sapling,
            orchard: &self.orchard,
            ironwood: &self.ironwood,
        }
    }
}

impl ZcashSerialize for TreeRoots {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.sapling.zcash_serialize(&mut writer)?;
        self.orchard.zcash_serialize(&mut writer)?;
        self.ironwood.zcash_serialize(&mut writer)
    }
}

impl ZcashDeserialize for TreeRoots {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self {
            sapling: sapling::tree::Root::zcash_deserialize(&mut reader)?,
            orchard: orchard::tree::Root::zcash_deserialize(&mut reader)?,
            ironwood: orchard::tree::Root::zcash_deserialize(&mut reader)?,
        })
    }
}
