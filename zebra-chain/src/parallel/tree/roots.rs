//! Note commitment tree roots after a block.

use std::{io, sync::LazyLock};

use crate::{
    block::merkle::AuthDataRoot,
    orchard,
    primitives::zcash_history::BlockCommitmentTreeRoots,
    sapling,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// The maximum number of blocks whose tree roots one request can ask for.
///
/// At most 516,000 bytes of proof entries, below the protocol's message size limit.
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

/// A draft fast-sync proof for the note commitment tree roots after a block.
///
/// From NU5, authenticating these roots against the next block's header also requires that
/// block's authorizing-data root. This digest comes from the root supplier, independently of
/// any separately downloaded successor body. Before NU5 it is absent.
///
/// This draft wire representation is not the persisted [`TreeRoots`] format and may change
/// before the tree-root protocol is specified by a ZIP.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TreeRootsProof {
    /// The Sapling, Orchard, and Ironwood roots after the requested block.
    pub roots: TreeRoots,
    /// The next block's authorizing-data root, required when its header uses ZIP-244.
    pub successor_auth_data_root: Option<AuthDataRoot>,
}

impl TreeRootsProof {
    /// The maximum serialized size: 96 root bytes, a presence byte, and a 32-byte digest.
    pub const MAX_SERIALIZED_SIZE: usize = TreeRoots::SERIALIZED_SIZE + 1 + 32;
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

impl ZcashSerialize for TreeRootsProof {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.roots.zcash_serialize(&mut writer)?;
        writer.write_all(&[u8::from(self.successor_auth_data_root.is_some())])?;
        if let Some(root) = self.successor_auth_data_root {
            writer.write_all(&<[u8; 32]>::from(root))?;
        }
        Ok(())
    }
}

impl ZcashDeserialize for TreeRootsProof {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let roots = TreeRoots::zcash_deserialize(&mut reader)?;
        let mut present = [0];
        reader.read_exact(&mut present)?;
        let successor_auth_data_root = match present[0] {
            0 => None,
            1 => {
                let mut root = [0; 32];
                reader.read_exact(&mut root)?;
                Some(root.into())
            }
            _ => {
                return Err(SerializationError::Parse(
                    "invalid successor auth root presence",
                ))
            }
        };
        Ok(Self {
            roots,
            successor_auth_data_root,
        })
    }
}
