//! Work computed for a block before its commit, so the commit can reuse it: the block's note
//! commitment trees, and for a mining candidate, its chain history and the result of its header
//! commitment check.
//!
//! A mining candidate computes the trees for a block right after its proof of work is checked.
//! The block's commit needs the same trees, so it reuses the candidate's result when the inputs
//! match, instead of repeating the Sinsemilla and Pedersen hashing.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, ChainHistoryMmrRootHash},
    history_tree::HistoryTree,
    parallel::tree::{NoteCommitmentTreeError, NoteCommitmentTrees},
};

/// The number of block validations that reused precomputed note commitment trees, for tests.
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) static REUSED_TREES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The number of mining candidates that got early work, for tests.
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) static MINING_CANDIDATES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The number of block validations that reused a mining candidate's history tree, for tests.
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) static REUSED_HISTORY_TREES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The number of block validations that skipped the header commitment check, because a mining
/// candidate had already checked it, for tests.
#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) static SKIPPED_COMMITMENT_CHECKS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The note commitment trees after a block, the parent trees they were computed from, and for a
/// mining candidate, its chain history after the block.
#[derive(Debug)]
pub(crate) struct PrecomputedTrees {
    /// The exact block body the trees were computed from.
    ///
    /// The trees are only reused for this same allocation, so a different body with the same
    /// header hash can never use them.
    block: Arc<Block>,

    /// The parent's trees and subtrees.
    input: NoteCommitmentTrees,

    /// The trees and subtrees after appending the block's note commitments to `input`.
    output: NoteCommitmentTrees,

    /// The chain history after the block, for a mining candidate whose header commitment was
    /// checked.
    history: Option<CandidateHistory>,
}

/// A mining candidate's chain history after its block.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CandidateHistory {
    /// The root of the parent's history tree, which the block's header commitment was
    /// checked against.
    parent_root: Option<ChainHistoryMmrRootHash>,

    /// The parent's history tree with the block pushed, using the roots of `output`.
    after: Arc<HistoryTree>,
}

impl PrecomputedTrees {
    /// Appends `block`'s note commitments to `input`, and returns both.
    pub(crate) fn compute(
        block: Arc<Block>,
        input: NoteCommitmentTrees,
    ) -> Result<Self, NoteCommitmentTreeError> {
        let mut output = input.clone();
        output.update_trees_parallel(&block)?;

        Ok(Self {
            block,
            input,
            output,
            history: None,
        })
    }

    /// Adds the chain history after the block, for a mining candidate whose header commitment
    /// was checked against a parent history tree with the root `parent_root`.
    ///
    /// `after` is that parent history tree with the block pushed, using the roots of the trees
    /// after the block.
    ///
    /// # Correctness
    ///
    /// The commit of the block skips its header commitment check if the block's parent history
    /// tree has the root `parent_root`. So this must only be called after that check passed for
    /// the block, against a history tree with that root.
    pub(crate) fn with_history(
        self,
        parent_root: Option<ChainHistoryMmrRootHash>,
        after: Arc<HistoryTree>,
    ) -> Self {
        Self {
            history: Some(CandidateHistory { parent_root, after }),
            ..self
        }
    }

    /// Returns the trees after the block.
    pub(crate) fn output(&self) -> &NoteCommitmentTrees {
        &self.output
    }

    /// Returns `true` if the trees were computed from the same body as `block`, and from `input`.
    fn computed_from(&self, block: &Arc<Block>, input: &NoteCommitmentTrees) -> bool {
        Arc::ptr_eq(&self.block, block) && &self.input == input
    }

    /// Returns the trees after `block` if they were computed from the same body and `input`.
    pub(crate) fn reuse(
        &self,
        block: &Arc<Block>,
        input: &NoteCommitmentTrees,
    ) -> Option<NoteCommitmentTrees> {
        self.computed_from(block, input)
            .then(|| self.output.clone())
    }

    /// Returns the history tree after `block` if the trees were computed from the same body and
    /// `input`, and the history tree after the block's parent has the same root as
    /// `parent_history`.
    ///
    /// The history tree after a block depends on the parent's history tree, the block, and the
    /// roots of the trees after the block. A history tree's root commits to the whole tree.
    pub(crate) fn reuse_history(
        &self,
        block: &Arc<Block>,
        input: &NoteCommitmentTrees,
        parent_history: &HistoryTree,
    ) -> Option<Arc<HistoryTree>> {
        let history = self.history.as_ref()?;

        (self.computed_from(block, input) && history.parent_root == parent_history.hash())
            .then(|| history.after.clone())
    }

    /// Returns `true` if `block`'s header commitment was checked against a parent history tree
    /// with the same root as `parent_history`.
    ///
    /// The check only depends on the block, and on the root of the history tree after the
    /// block's parent.
    pub(crate) fn commitment_is_checked(
        &self,
        block: &Arc<Block>,
        parent_history: &HistoryTree,
    ) -> bool {
        Arc::ptr_eq(&self.block, block)
            && self
                .history
                .as_ref()
                .is_some_and(|history| history.parent_root == parent_history.hash())
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::{
        history_tree::NonEmptyHistoryTree, parameters::Network,
        primitives::zcash_history::BlockCommitmentTreeRoots, serialization::ZcashDeserializeInto,
    };

    use super::*;

    fn orchard_block() -> Arc<Block> {
        zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES
            .zcash_deserialize_into()
            .expect("the test vector is a valid block")
    }

    /// Returns a history tree that starts at `block`, to stand in for the history tree after a
    /// block's parent. Its root isn't the root of an empty history tree.
    fn parent_history(block: &Arc<Block>) -> HistoryTree {
        parent_history_with_roots(block, &NoteCommitmentTrees::default())
    }

    /// Returns a history tree that starts at `block`, with the roots of `trees`.
    fn parent_history_with_roots(block: &Arc<Block>, trees: &NoteCommitmentTrees) -> HistoryTree {
        NonEmptyHistoryTree::from_block(
            &Network::Mainnet,
            block.clone(),
            BlockCommitmentTreeRoots {
                sapling: &trees.sapling.root(),
                orchard: &trees.orchard.root(),
                ironwood: &trees.ironwood.root(),
            },
        )
        .expect("a history tree can start at an NU5 block")
        .into()
    }

    /// Reused trees are exactly the trees the commit would compute from the same input.
    #[test]
    fn reuse_matches_computed_trees() {
        let _init_guard = zebra_test::init();

        let block = orchard_block();
        assert!(block.orchard_note_commitments().next().is_some());

        let input = NoteCommitmentTrees::default();
        let precomputed = PrecomputedTrees::compute(block.clone(), input.clone())
            .expect("a block's commitments fit an empty tree");

        let mut computed = input.clone();
        computed.update_trees_parallel(&block).unwrap();

        assert_eq!(precomputed.reuse(&block, &input), Some(computed));
    }

    /// Trees are never reused for another body or other parent trees.
    #[test]
    fn reuse_requires_same_body_and_input() {
        let _init_guard = zebra_test::init();

        let block = orchard_block();
        let input = NoteCommitmentTrees::default();
        let precomputed = PrecomputedTrees::compute(block.clone(), input.clone())
            .expect("a block's commitments fit an empty tree");

        let same_bytes = orchard_block();
        assert_eq!(same_bytes, block);
        assert_eq!(
            precomputed.reuse(&same_bytes, &input),
            None,
            "a separately received body can differ from the computed one in ways the hash misses",
        );

        let other_input = precomputed.output().clone();
        assert_eq!(precomputed.reuse(&block, &other_input), None);
    }

    /// A mining candidate's history tree is only reused for the same body, computed from the same
    /// parent trees, on a parent history tree with the same root.
    #[test]
    fn history_is_reused_only_for_its_block_inputs_and_parent_root() {
        let _init_guard = zebra_test::init();

        let block = orchard_block();
        let input = NoteCommitmentTrees::default();
        let parent_history = parent_history(&block);
        let after = Arc::new(HistoryTree::default());

        let precomputed = PrecomputedTrees::compute(block.clone(), input.clone())
            .expect("a block's commitments fit an empty tree");
        assert_eq!(
            precomputed.reuse_history(&block, &input, &parent_history),
            None,
            "trees without a chain history have no history to reuse",
        );

        let precomputed = precomputed.with_history(parent_history.hash(), after.clone());
        let reused = precomputed
            .reuse_history(&block, &input, &parent_history)
            .expect("the history is reused for its own body, inputs and parent root");
        assert!(Arc::ptr_eq(&reused, &after));

        let same_bytes = orchard_block();
        assert_eq!(same_bytes, block);
        assert_eq!(
            precomputed.reuse_history(&same_bytes, &input, &parent_history),
            None,
            "a separately received body can differ from the computed one in ways the hash misses",
        );

        let other_input = precomputed.output().clone();
        assert_eq!(
            precomputed.reuse_history(&block, &other_input, &parent_history),
            None,
            "other parent trees give the block other tree roots",
        );

        assert_eq!(
            precomputed.reuse_history(&block, &input, &HistoryTree::default()),
            None,
            "a parent history tree with another root gives another history tree after the block",
        );

        let other_parent_history = parent_history_with_roots(&block, precomputed.output());
        assert!(other_parent_history.hash().is_some());
        assert_ne!(other_parent_history.hash(), parent_history.hash());
        assert_eq!(
            precomputed.reuse_history(&block, &input, &other_parent_history),
            None,
            "another non-empty parent root also gives another history tree after the block",
        );
    }

    /// A mining candidate's header commitment check is only reused for the same body, on a
    /// parent history tree with the same root.
    #[test]
    fn commitment_check_is_reused_only_for_its_block_and_parent_root() {
        let _init_guard = zebra_test::init();

        let block = orchard_block();
        let parent_history = parent_history(&block);

        let precomputed = PrecomputedTrees::compute(block.clone(), NoteCommitmentTrees::default())
            .expect("a block's commitments fit an empty tree");
        assert!(
            !precomputed.commitment_is_checked(&block, &parent_history),
            "trees without a chain history don't record a commitment check",
        );

        let precomputed =
            precomputed.with_history(parent_history.hash(), Arc::new(HistoryTree::default()));
        assert!(precomputed.commitment_is_checked(&block, &parent_history));

        let same_bytes = orchard_block();
        assert_eq!(same_bytes, block);
        assert!(
            !precomputed.commitment_is_checked(&same_bytes, &parent_history),
            "a separately received body can differ from the checked one in ways the hash misses",
        );

        assert!(
            !precomputed.commitment_is_checked(&block, &HistoryTree::default()),
            "the commitment depends on the root of the parent's history tree",
        );

        let other_parent_history = parent_history_with_roots(&block, precomputed.output());
        assert!(other_parent_history.hash().is_some());
        assert_ne!(other_parent_history.hash(), parent_history.hash());
        assert!(
            !precomputed.commitment_is_checked(&block, &other_parent_history),
            "another non-empty parent root needs another commitment",
        );
    }
}
