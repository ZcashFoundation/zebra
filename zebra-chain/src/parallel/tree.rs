//! Parallel note commitment tree update methods.

use std::sync::Arc;

use thiserror::Error;

use crate::{block::Block, orchard, sapling, sprout};

/// An argument wrapper struct for note commitment trees.
#[derive(Clone, Debug)]
pub struct NoteCommitmentTrees {
    /// The sprout note commitment tree.
    pub sprout: Arc<sprout::tree::NoteCommitmentTree>,

    /// The sapling note commitment tree.
    pub sapling: Arc<sapling::tree::NoteCommitmentTree>,

    /// The orchard note commitment tree.
    pub orchard: Arc<orchard::tree::NoteCommitmentTree>,
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
    /// Update these note commitment trees for the entire `block`,
    /// using parallel `rayon` threads.
    ///
    /// If any of the tree updates cause an error,
    /// it will be returned at the end of the parallel batches.
    pub fn update_trees_parallel(
        &mut self,
        block: &Arc<Block>,
    ) -> Result<(), NoteCommitmentTreeError> {
        // Prepare arguments for parallel threads
        let NoteCommitmentTrees {
            sprout,
            sapling,
            orchard,
        } = self.clone();

        let sprout_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.sprout_note_commitments())
            .cloned()
            .collect();
        let sapling_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.sapling_note_commitments())
            .cloned()
            .collect();
        let orchard_note_commitments: Vec<_> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.orchard_note_commitments())
            .cloned()
            .collect();

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
            self.sapling = sapling_result?;
        }
        if let Some(orchard_result) = orchard_result {
            self.orchard = orchard_result?;
        }

        Ok(())
    }

    /// Update the sprout note commitment tree.
    fn update_sprout_note_commitment_tree(
        mut sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sprout_note_commitments: Vec<sprout::NoteCommitment>,
    ) -> Result<Arc<sprout::tree::NoteCommitmentTree>, NoteCommitmentTreeError> {
        let sprout_nct = Arc::make_mut(&mut sprout);

        for sprout_note_commitment in sprout_note_commitments {
            sprout_nct.append(sprout_note_commitment)?;
        }

        Ok(sprout)
    }

    /// Update the sapling note commitment tree.
    fn update_sapling_note_commitment_tree(
        mut sapling: Arc<sapling::tree::NoteCommitmentTree>,
        sapling_note_commitments: Vec<sapling::tree::NoteCommitmentUpdate>,
    ) -> Result<Arc<sapling::tree::NoteCommitmentTree>, NoteCommitmentTreeError> {
        let sapling_nct = Arc::make_mut(&mut sapling);

        for sapling_note_commitment in sapling_note_commitments {
            sapling_nct.append(sapling_note_commitment)?;
        }

        Ok(sapling)
    }

    /// Update the orchard note commitment tree.
    fn update_orchard_note_commitment_tree(
        mut orchard: Arc<orchard::tree::NoteCommitmentTree>,
        orchard_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<Arc<orchard::tree::NoteCommitmentTree>, NoteCommitmentTreeError> {
        let orchard_nct = Arc::make_mut(&mut orchard);

        for orchard_note_commitment in orchard_note_commitments {
            orchard_nct.append(orchard_note_commitment)?;
        }

        Ok(orchard)
    }
}
