//! Orchard and Ironwood note commitment tree updates that are computed before the block that
//! makes them arrives.

use std::sync::Arc;

use halo2::pasta::{group::ff::PrimeField, pallas};

use crate::{orchard, subtree::NoteCommitmentSubtreeIndex, transaction::Transaction};

use super::{NoteCommitmentTreeError, NoteCommitmentTrees};

/// An updated tree, and the subtree it completed, if any.
pub(super) type TreeUpdate = (
    Arc<orchard::tree::NoteCommitmentTree>,
    Option<(NoteCommitmentSubtreeIndex, orchard::tree::Node)>,
);

/// The result of appending note commitments to an Orchard or Ironwood note commitment tree.
///
/// Calculating the root of a tree takes one Sinsemilla hash for each of its 32 levels, which is
/// most of the time a block with a few shielded transactions takes to validate in its chain
/// context. An update that was computed when its transactions were first seen saves that time
/// when a block appends the same note commitments to the same tree.
///
/// The update is a function of the parent tree and the note commitments only, so reusing it
/// gives the tree that the block's own update would.
#[derive(Clone, Debug)]
pub struct PrecomputedTreeUpdate {
    /// The tree the note commitments were appended to.
    parent: Arc<orchard::tree::NoteCommitmentTree>,

    /// The appended note commitments, in order.
    note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,

    /// The updated tree, with its root, and the subtree the update completed.
    update: TreeUpdate,
}

impl PrecomputedTreeUpdate {
    /// Appends `note_commitments` to `parent`, and calculates the root of the updated tree.
    pub fn new(
        parent: Arc<orchard::tree::NoteCommitmentTree>,
        note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<Self, NoteCommitmentTreeError> {
        let update = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            parent.clone(),
            note_commitments.clone(),
        )?;

        Ok(Self {
            parent,
            note_commitments,
            update,
        })
    }

    /// Returns the updated tree.
    pub fn tree(&self) -> &Arc<orchard::tree::NoteCommitmentTree> {
        &self.update.0
    }

    /// Returns the note commitments that a block with `transaction` appends to the Orchard tree
    /// for it.
    pub fn orchard_note_commitments(
        transaction: &Transaction,
    ) -> Vec<orchard::tree::NoteCommitmentUpdate> {
        transaction
            .orchard_note_commitments()
            .map(|cmx| {
                pallas::Base::from_repr(cmx.to_bytes())
                    .expect("orchard note commitment is a valid pallas::Base")
            })
            .collect()
    }

    /// Returns the note commitments that a block with `transaction` appends to the Ironwood
    /// tree for it.
    pub fn ironwood_note_commitments(
        transaction: &Transaction,
    ) -> Vec<orchard::tree::NoteCommitmentUpdate> {
        transaction
            .ironwood_note_commitments()
            .map(|cmx| {
                pallas::Base::from_repr(cmx.to_bytes())
                    .expect("ironwood note commitment is a valid pallas::Base")
            })
            .collect()
    }

    /// Returns the update of `parent` with `note_commitments` from `precomputed`, if it is there.
    ///
    /// An update is only returned for the same tree and the same note commitments in the same
    /// order. The trees are compared by their contents, not by their roots.
    pub fn find(
        precomputed: &[Arc<PrecomputedTreeUpdate>],
        parent: &Arc<orchard::tree::NoteCommitmentTree>,
        note_commitments: &[orchard::tree::NoteCommitmentUpdate],
    ) -> Option<TreeUpdate> {
        precomputed
            .iter()
            .find(|precomputed| {
                precomputed.note_commitments == note_commitments
                    && (Arc::ptr_eq(&precomputed.parent, parent)
                        || precomputed.parent.has_same_frontier(parent))
            })
            .map(|precomputed| precomputed.update.clone())
    }
}
