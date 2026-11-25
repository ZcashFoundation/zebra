//! Note commitment trees computed for a block before its commit, so the commit can reuse them.
//!
//! A mining candidate computes the trees for a block right after its proof of work is checked.
//! The block's commit needs the same trees, so it reuses the candidate's result when the inputs
//! match, instead of repeating the Sinsemilla and Pedersen hashing.

use std::sync::Arc;

use zebra_chain::{
    block::Block,
    parallel::tree::{NoteCommitmentTreeError, NoteCommitmentTrees},
};

/// The number of block commits that reused precomputed trees, so tests can check that a commit
/// didn't compute the trees again.
#[cfg(test)]
pub(crate) static REUSED_TREES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The note commitment trees after a block, and the parent trees they were computed from.
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
        })
    }

    /// Returns the trees after the block.
    pub(crate) fn output(&self) -> &NoteCommitmentTrees {
        &self.output
    }

    /// Returns the trees after `block` if they were computed from the same body and `input`.
    pub(crate) fn reuse(
        &self,
        block: &Arc<Block>,
        input: &NoteCommitmentTrees,
    ) -> Option<NoteCommitmentTrees> {
        (Arc::ptr_eq(&self.block, block) && &self.input == input).then(|| self.output.clone())
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::serialization::ZcashDeserializeInto;

    use super::*;

    fn orchard_block() -> Arc<Block> {
        zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES
            .zcash_deserialize_into()
            .expect("the test vector is a valid block")
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
}
