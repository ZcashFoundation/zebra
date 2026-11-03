//! Parallel note commitment tree update methods.

use std::sync::Arc;

use thiserror::Error;

use crate::{
    block::Block,
    orchard, sapling, sprout,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
};

#[cfg(test)]
mod tests;

/// Counts note commitment tree update attempts in tests that enable `proptest-impl`.
#[cfg(any(test, feature = "proptest-impl"))]
pub static NOTE_COMMITMENT_TREE_UPDATE_COUNT_FOR_TESTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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

    /// The ironwood note commitment tree (NU6.3).
    ///
    /// Ironwood reuses the Orchard note commitment tree type (same Pallas/Sinsemilla MerkleCRH),
    /// but commits Ironwood-pool notes into a separate tree. Stays empty until NU6.3.
    pub ironwood: Arc<orchard::tree::NoteCommitmentTree>,

    /// The ironwood note commitment subtree (NU6.3).
    pub ironwood_subtree: Option<NoteCommitmentSubtree<orchard::tree::Node>>,
}

/// Runs `job` on the calling thread if `run_here` is true, or spawns it on `scope` otherwise.
fn spawn_or_run_here<'scope, F>(scope: &rayon::ScopeFifo<'scope>, run_here: bool, job: F)
where
    F: FnOnce(&rayon::ScopeFifo<'scope>) + Send + 'scope,
{
    if run_here {
        job(scope);
    } else {
        scope.spawn_fifo(job);
    }
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

    /// An ironwood tree error
    ///
    /// Ironwood reuses the Orchard tree type, so this wraps the same error type as
    /// [`Self::Orchard`]; the distinct variant keeps the pool that failed identifiable.
    #[error("ironwood error: {0}")]
    Ironwood(#[source] orchard::tree::NoteCommitmentTreeError),
}

impl NoteCommitmentTrees {
    /// Updates the note commitment trees using the transactions in `block`,
    /// then re-calculates the cached tree roots, using parallel `rayon` threads.
    ///
    /// If any of the tree updates cause an error,
    /// it will be returned at the end of the parallel batches.
    pub fn update_trees_parallel(
        &mut self,
        block: &Arc<Block>,
    ) -> Result<(), NoteCommitmentTreeError> {
        self.update_trees_parallel_with_cancel(block, || false)
            .map(|_completed| ())
    }

    /// Updates the note commitment trees in parallel until `is_cancelled` returns true.
    ///
    /// Returns `false` if any pool update was cancelled. In that case, `self` can be partially
    /// updated and must be discarded in favour of the error that triggered the cancellation.
    pub fn update_trees_parallel_with_cancel<F>(
        &mut self,
        block: &Arc<Block>,
        is_cancelled: F,
    ) -> Result<bool, NoteCommitmentTreeError>
    where
        F: Fn() -> bool + Sync,
    {
        if is_cancelled() {
            return Ok(false);
        }

        let block = block.clone();
        let height = block
            .coinbase_height()
            .expect("height was already validated");

        // Prepare arguments for parallel threads
        let NoteCommitmentTrees {
            sprout,
            sapling,
            orchard,
            ironwood,
            ..
        } = self.clone();

        let sprout_note_commitments: Vec<_> = block.sprout_note_commitments().collect();
        let sapling_note_commitments: Vec<_> = block.sapling_note_commitments().collect();
        let orchard_note_commitments: Vec<_> = block.orchard_note_commitments().collect();
        let ironwood_note_commitments: Vec<_> = block.ironwood_note_commitments().collect();

        #[cfg(any(test, feature = "proptest-impl"))]
        NOTE_COMMITMENT_TREE_UPDATE_COUNT_FOR_TESTS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let mut sprout_result = None;
        let mut sapling_result = None;
        let mut orchard_result = None;
        let mut ironwood_result = None;

        // The costliest pool with commitments runs on the calling thread, so that thread hashes
        // instead of waiting for a rayon thread to start the job. Orchard and Ironwood hash with
        // Sinsemilla, which costs more than Sapling's Pedersen hash, and Sprout's SHA-256 costs
        // the least.
        let run_orchard_here = !orchard_note_commitments.is_empty();
        let run_ironwood_here = !run_orchard_here && !ironwood_note_commitments.is_empty();
        let run_sapling_here =
            !run_orchard_here && !run_ironwood_here && !sapling_note_commitments.is_empty();
        let run_sprout_here = !run_orchard_here && !run_ironwood_here && !run_sapling_here;

        // Pools are started from the cheapest to the costliest, so the pool that runs on this
        // thread starts after the others are spawned.
        rayon::in_place_scope_fifo(|scope| {
            if !sprout_note_commitments.is_empty() {
                spawn_or_run_here(scope, run_sprout_here, |_scope| {
                    sprout_result = Some(Self::update_sprout_note_commitment_tree_with_cancel(
                        sprout,
                        sprout_note_commitments,
                        &is_cancelled,
                    ));
                });
            }

            if !sapling_note_commitments.is_empty() {
                spawn_or_run_here(scope, run_sapling_here, |_scope| {
                    sapling_result = Some(Self::update_sapling_note_commitment_tree_with_cancel(
                        sapling,
                        sapling_note_commitments,
                        &is_cancelled,
                    ));
                });
            }

            if !ironwood_note_commitments.is_empty() {
                spawn_or_run_here(scope, run_ironwood_here, |_scope| {
                    ironwood_result = Some(
                        Self::update_orchard_note_commitment_tree_with_cancel(
                            ironwood,
                            ironwood_note_commitments,
                            &is_cancelled,
                        )
                        .map_err(|err| match err {
                            NoteCommitmentTreeError::Orchard(inner) => {
                                NoteCommitmentTreeError::Ironwood(inner)
                            }
                            other => other,
                        }),
                    );
                });
            }

            if !orchard_note_commitments.is_empty() {
                spawn_or_run_here(scope, run_orchard_here, |_scope| {
                    orchard_result = Some(Self::update_orchard_note_commitment_tree_with_cancel(
                        orchard,
                        orchard_note_commitments,
                        &is_cancelled,
                    ));
                });
            }
        });

        let mut completed = true;

        if let Some(sprout_result) = sprout_result {
            match sprout_result? {
                Some(sprout) => self.sprout = sprout,
                None => completed = false,
            }
        }

        if let Some(sapling_result) = sapling_result {
            match sapling_result? {
                Some((sapling, subtree_root)) => {
                    self.sapling = sapling;
                    self.sapling_subtree = subtree_root
                        .map(|(idx, node)| NoteCommitmentSubtree::new(idx, height, node));
                }
                None => completed = false,
            }
        };

        if let Some(orchard_result) = orchard_result {
            match orchard_result? {
                Some((orchard, subtree_root)) => {
                    self.orchard = orchard;
                    self.orchard_subtree = subtree_root
                        .map(|(idx, node)| NoteCommitmentSubtree::new(idx, height, node));
                }
                None => completed = false,
            }
        };

        if let Some(ironwood_result) = ironwood_result {
            match ironwood_result? {
                Some((ironwood, subtree_root)) => {
                    self.ironwood = ironwood;
                    self.ironwood_subtree = subtree_root
                        .map(|(idx, node)| NoteCommitmentSubtree::new(idx, height, node));
                }
                None => completed = false,
            }
        };

        Ok(completed)
    }

    /// Update the sprout note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    #[cfg(test)]
    #[allow(clippy::unwrap_in_result)]
    fn update_sprout_note_commitment_tree(
        sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sprout_note_commitments: Vec<sprout::NoteCommitment>,
    ) -> Result<Arc<sprout::tree::NoteCommitmentTree>, NoteCommitmentTreeError> {
        Self::update_sprout_note_commitment_tree_with_cancel(
            sprout,
            sprout_note_commitments,
            || false,
        )
        .map(|result| result.expect("a non-cancellable tree update cannot be cancelled"))
    }

    fn update_sprout_note_commitment_tree_with_cancel<F>(
        mut sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sprout_note_commitments: Vec<sprout::NoteCommitment>,
        is_cancelled: F,
    ) -> Result<Option<Arc<sprout::tree::NoteCommitmentTree>>, NoteCommitmentTreeError>
    where
        F: Fn() -> bool + Sync,
    {
        if is_cancelled() {
            return Ok(None);
        }

        let sprout_nct = Arc::make_mut(&mut sprout);

        for sprout_note_commitment in sprout_note_commitments {
            if is_cancelled() {
                return Ok(None);
            }
            sprout_nct.append(sprout_note_commitment)?;
        }

        if is_cancelled() {
            return Ok(None);
        }

        // Re-calculate and cache the tree root.
        let _ = sprout_nct.root();

        Ok(Some(sprout))
    }

    /// Update the sapling note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    #[allow(clippy::unwrap_in_result)]
    pub fn update_sapling_note_commitment_tree(
        sapling: Arc<sapling::tree::NoteCommitmentTree>,
        sapling_note_commitments: Vec<sapling::tree::NoteCommitmentUpdate>,
    ) -> Result<
        (
            Arc<sapling::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, sapling_crypto::Node)>,
        ),
        NoteCommitmentTreeError,
    > {
        Self::update_sapling_note_commitment_tree_with_cancel(
            sapling,
            sapling_note_commitments,
            || false,
        )
        .map(|result| result.expect("a non-cancellable tree update cannot be cancelled"))
    }

    #[allow(clippy::unwrap_in_result)]
    fn update_sapling_note_commitment_tree_with_cancel<F>(
        mut sapling: Arc<sapling::tree::NoteCommitmentTree>,
        sapling_note_commitments: Vec<sapling::tree::NoteCommitmentUpdate>,
        is_cancelled: F,
    ) -> Result<
        Option<(
            Arc<sapling::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, sapling_crypto::Node)>,
        )>,
        NoteCommitmentTreeError,
    >
    where
        F: Fn() -> bool + Sync,
    {
        if is_cancelled() {
            return Ok(None);
        }

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
        let subtree_root =
            match sapling_nct.append_batch_with_cancel(&sapling_note_commitments, &is_cancelled) {
                Ok(subtree_root) => subtree_root,
                Err(super::batch_frontier::AppendBatchError::Frontier) => {
                    return Err(sapling::tree::NoteCommitmentTreeError::FullTree.into())
                }
                Err(super::batch_frontier::AppendBatchError::Cancelled) => return Ok(None),
            };

        if is_cancelled() {
            return Ok(None);
        }

        // Re-calculate and cache the tree root.
        let _ = sapling_nct.root();

        Ok(Some((sapling, subtree_root)))
    }

    /// Update the orchard note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    #[allow(clippy::unwrap_in_result)]
    pub fn update_orchard_note_commitment_tree(
        orchard: Arc<orchard::tree::NoteCommitmentTree>,
        orchard_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<
        (
            Arc<orchard::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, orchard::tree::Node)>,
        ),
        NoteCommitmentTreeError,
    > {
        Self::update_orchard_note_commitment_tree_with_cancel(
            orchard,
            orchard_note_commitments,
            || false,
        )
        .map(|result| result.expect("a non-cancellable tree update cannot be cancelled"))
    }

    #[allow(clippy::unwrap_in_result)]
    fn update_orchard_note_commitment_tree_with_cancel<F>(
        mut orchard: Arc<orchard::tree::NoteCommitmentTree>,
        orchard_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
        is_cancelled: F,
    ) -> Result<
        Option<(
            Arc<orchard::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, orchard::tree::Node)>,
        )>,
        NoteCommitmentTreeError,
    >
    where
        F: Fn() -> bool + Sync,
    {
        if is_cancelled() {
            return Ok(None);
        }

        let orchard_nct = Arc::make_mut(&mut orchard);

        // It is impossible for blocks to contain more than one level 16 orchard root:
        // > [NU5 onward] nSpendsSapling, nOutputsSapling, and nActionsOrchard MUST all be less than 2^16.
        // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
        let subtree_root =
            match orchard_nct.append_batch_with_cancel(&orchard_note_commitments, &is_cancelled) {
                Ok(subtree_root) => subtree_root,
                Err(super::batch_frontier::AppendBatchError::Frontier) => {
                    return Err(orchard::tree::NoteCommitmentTreeError::FullTree.into())
                }
                Err(super::batch_frontier::AppendBatchError::Cancelled) => return Ok(None),
            };

        if is_cancelled() {
            return Ok(None);
        }

        // Re-calculate and cache the tree root.
        let _ = orchard_nct.root();

        Ok(Some((orchard, subtree_root)))
    }

    /// Update the ironwood note commitment tree.
    /// This method modifies the tree inside the `Arc`, if the `Arc` only has one reference.
    ///
    /// Ironwood reuses the Orchard note commitment tree type but commits into a separate tree, so
    /// this delegates to [`Self::update_orchard_note_commitment_tree`] and only re-tags the error
    /// variant as [`NoteCommitmentTreeError::Ironwood`].
    #[allow(clippy::unwrap_in_result)]
    pub fn update_ironwood_note_commitment_tree(
        ironwood: Arc<orchard::tree::NoteCommitmentTree>,
        ironwood_note_commitments: Vec<orchard::tree::NoteCommitmentUpdate>,
    ) -> Result<
        (
            Arc<orchard::tree::NoteCommitmentTree>,
            Option<(NoteCommitmentSubtreeIndex, orchard::tree::Node)>,
        ),
        NoteCommitmentTreeError,
    > {
        Self::update_orchard_note_commitment_tree(ironwood, ironwood_note_commitments).map_err(
            |err| match err {
                NoteCommitmentTreeError::Orchard(inner) => NoteCommitmentTreeError::Ironwood(inner),
                other => other,
            },
        )
    }
}
