//! Parallel note commitment tree update methods.

use std::sync::Arc;

use thiserror::Error;

use crate::{
    block::Block,
    orchard, sapling, sprout,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
};

/// An argument wrapper struct for note commitment trees.
///
/// The default instance represents the trees and subtrees that correspond to the genesis block.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NoteCommitmentTrees {
    /// The sprout note commitment tree.
    pub sprout: Arc<sprout::tree::NoteCommitmentTree>,

    /// The sapling note commitment tree.
    pub sapling: Arc<sapling::tree::NoteCommitmentTree>,

    /// The sapling note commitment subtree.
    pub sapling_subtree: Option<NoteCommitmentSubtree<sapling_crypto::Node>>,

    /// The orchard note commitment tree.
    pub orchard: Arc<orchard::tree::NoteCommitmentTree>,

    /// The orchard note commitment subtree.
    pub orchard_subtree: Option<NoteCommitmentSubtree<orchard::tree::Node>>,
}

/// Note commitment tree errors.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NoteCommitmentTreeError {
    /// A sprout tree error
    #[error("sprout error: {0}")]
    Sprout(#[from] sprout::tree::NoteCommitmentTreeError),

    /// A sapling tree error
    #[error("sapling error: {0}")]
    Sapling(#[from] sapling::tree::NoteCommitmentTreeError),

    /// A orchard tree error
    #[error("orchard error: {0}")]
    Orchard(#[from] orchard::tree::NoteCommitmentTreeError),
}

impl NoteCommitmentTrees {
    /// Updates the note commitment trees using the transactions in `block`,
    /// then re-calculates the cached tree roots, using parallel `rayon` threads.
    ///
    /// If any of the tree updates cause an error,
    /// it will be returned at the end of the parallel batches.
    #[allow(clippy::unwrap_in_result)]
    pub fn update_trees_parallel(
        &mut self,
        block: &Arc<Block>,
    ) -> Result<(), NoteCommitmentTreeError> {
        let block = block.clone();
        let height = block
            .coinbase_height()
            .expect("height was already validated");

        // Prepare arguments for parallel threads
        let NoteCommitmentTrees {
            sprout,
            sapling,
            orchard,
            ..
        } = self.clone();

        let sprout_note_commitments: Vec<_> = block.sprout_note_commitments().cloned().collect();
        let sapling_note_commitments: Vec<_> = block.sapling_note_commitments().cloned().collect();
        let orchard_note_commitments: Vec<_> = block.orchard_note_commitments().cloned().collect();

        let mut sprout_result = None;
        let mut sapling_result = None;
        let mut orchard_result = None;

        rayon::in_place_scope_fifo(|scope| {
            if !sprout_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    sprout_result = Some(Self::update_sprout_note_commitment_tree(
                        sprout,
                        sprout_note_commitments,
                    ));
                });
            }

            if !sapling_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    sapling_result = Some(Self::update_sapling_note_commitment_tree(
                        sapling,
                        sapling_note_commitments,
                    ));
                });
            }

            if !orchard_note_commitments.is_empty() {
                scope.spawn_fifo(|_scope| {
                    orchard_result = Some(Self::update_orchard_note_commitment_tree(
                        orchard,
                        orchard_note_commitments,
                    ));
                });
            }
        });

        if let Some(sprout_result) = sprout_result {
            self.sprout = sprout_result?;
        }

        if let Some(sapling_result) = sapling_result {
            let (sapling, subtree_root) = sapling_result?;
            self.sapling = sapling;
            self.sapling_subtree =
                subtree_root.map(|(idx, node)| NoteCommitmentSubtree::new(idx, height, node));
        };

        if let Some(orchard_result) = orchard_result {
            let (orchard, subtree_root) = orchard_result?;
            self.orchard = orchard;
            self.orchard_subtree =
                subtree_root.map(|(idx, node)| NoteCommitmentSubtree::new(idx, height, node));
        };

        Ok(())
    }

    /// Update the sprout note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    fn update_sprout_note_commitment_tree(
        mut sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sprout_note_commitments: Vec<sprout::NoteCommitment>,
    ) -> Result<Arc<sprout::tree::NoteCommitmentTree>, NoteCommitmentTreeError> {
        let sprout_nct = Arc::make_mut(&mut sprout);

        for sprout_note_commitment in sprout_note_commitments {
            sprout_nct.append(sprout_note_commitment)?;
        }

        // Re-calculate and cache the tree root.
        let _ = sprout_nct.root();

        Ok(sprout)
    }

    /// Update the sapling note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    #[allow(clippy::unwrap_in_result)]
    pub fn update_sapling_note_commitment_tree(
        mut sapling: Arc<sapling::tree::NoteCommitmentTree>,
        sapling_note_commitments: Vec<sapling::tree::NoteCommitmentUpdate>,
    ) -> Result<
        (
            Arc<sapling::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, sapling_crypto::Node)>,
        ),
        NoteCommitmentTreeError,
    > {
        let sapling_nct = Arc::make_mut(&mut sapling);

        // It is impossible for blocks to contain more than one level 16 sapling root:
        // > [NU5 onward] nSpendsSapling, nOutputsSapling, and nActionsOrchard MUST all be less than 2^16.
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        //
        // Before NU5, this limit holds due to the minimum size of Sapling outputs (948 bytes)
        // and the maximum size of a block:
        // > The size of a block MUST be less than or equal to 2000000 bytes.
        // <https://zips.z.cash/protocol/protocol.pdf#blockheader>
        // <https://zips.z.cash/protocol/protocol.pdf#txnencoding>
        let mut subtree_root = None;

        for sapling_note_commitment in sapling_note_commitments {
            sapling_nct.append(sapling_note_commitment)?;

            // Subtrees end heights come from the blocks they are completed in,
            // so we check for new subtrees after appending the note.
            // (If we check before, subtrees at the end of blocks have the wrong heights.)
            if let Some(index_and_node) = sapling_nct.completed_subtree_index_and_root() {
                subtree_root = Some(index_and_node);
            }
        }

        // Re-calculate and cache the tree root.
        let _ = sapling_nct.root();

        Ok((sapling, subtree_root))
    }

    /// Update the orchard note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    #[allow(clippy::unwrap_in_result)]
    pub fn update_orchard_note_commitment_tree(
        mut orchard: Arc<orchard::tree::NoteCommitmentTree>,
        orchard_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<
        (
            Arc<orchard::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, orchard::tree::Node)>,
        ),
        NoteCommitmentTreeError,
    > {
        let orchard_nct = Arc::make_mut(&mut orchard);

        // It is impossible for blocks to contain more than one level 16 orchard root:
        // > [NU5 onward] nSpendsSapling, nOutputsSapling, and nActionsOrchard MUST all be less than 2^16.
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let mut subtree_root = None;

        for orchard_note_commitment in orchard_note_commitments {
            orchard_nct.append(orchard_note_commitment)?;

            // Subtrees end heights come from the blocks they are completed in,
            // so we check for new subtrees after appending the note.
            // (If we check before, subtrees at the end of blocks have the wrong heights.)
            if let Some(index_and_node) = orchard_nct.completed_subtree_index_and_root() {
                subtree_root = Some(index_and_node);
            }
        }

        // Re-calculate and cache the tree root.
        let _ = orchard_nct.root();

        Ok((orchard, subtree_root))
    }
}
