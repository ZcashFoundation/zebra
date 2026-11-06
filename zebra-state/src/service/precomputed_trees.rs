//! Note commitment trees computed for a block before its commit, so the commit can reuse them.
//!
//! A mining candidate computes the trees for a block right after its proof of work is checked.
//! The block's commit computes the same trees, so it reuses the candidate's result when the
//! inputs match, instead of repeating the Sinsemilla hashing.

use std::{sync::Arc, time::Duration};

use tokio::sync::watch;
use zebra_chain::{block::Block, parallel::tree::NoteCommitmentTrees};

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
    /// Appends `block`'s note commitments to `input`, and returns both, or `None` on a tree error.
    pub(crate) fn compute(block: Arc<Block>, input: NoteCommitmentTrees) -> Option<Self> {
        let mut output = input.clone();
        output.update_trees_parallel(&block).ok()?;
        Some(Self {
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

/// The latest note commitment tree computation for a block that hasn't been committed yet.
#[derive(Clone, Debug, Default)]
pub(crate) enum TreeSlot {
    /// No computation is running or available.
    #[default]
    Empty,

    /// Trees are being computed for this block body.
    Computing(Arc<Block>),

    /// Trees were computed for a block body.
    Ready(Arc<PrecomputedTrees>),
}

/// Marks a block's trees as being computed, and publishes them, or an empty slot, when dropped.
pub(crate) struct TreeSlotGuard {
    slot: watch::Sender<TreeSlot>,
    block: Arc<Block>,
    trees: Option<Arc<PrecomputedTrees>>,
}

impl TreeSlotGuard {
    /// Marks the slot as computing trees for `block`, so its commit waits for them.
    ///
    /// Dropping the guard without [`Self::finish`] empties the slot, which releases the commit.
    pub(crate) fn start(slot: watch::Sender<TreeSlot>, block: Arc<Block>) -> Self {
        slot.send_replace(TreeSlot::Computing(block.clone()));
        Self {
            slot,
            block,
            trees: None,
        }
    }

    /// Publishes the computed trees.
    pub(crate) fn finish(mut self, trees: Arc<PrecomputedTrees>) {
        self.trees = Some(trees);
    }
}

impl Drop for TreeSlotGuard {
    fn drop(&mut self) {
        let trees = self.trees.take();
        self.slot.send_if_modified(|slot| {
            if !matches!(slot, TreeSlot::Computing(computing) if Arc::ptr_eq(computing, &self.block))
            {
                return false;
            }
            *slot = trees.map_or(TreeSlot::Empty, TreeSlot::Ready);
            true
        });
    }
}

/// The longest a commit waits for a mining candidate's trees before computing them itself.
///
/// A candidate computes them in a few milliseconds, so this only bounds a stalled computation.
pub(crate) const TREE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Waits for a running computation for `block`, then takes its trees if they're available.
///
/// Doesn't wait if no computation for `block` is running. Empties the slot, so finished trees
/// aren't retained after their commit, and trees for another block aren't retained either.
///
/// Called from the block write thread, which is outside the async runtime and outside rayon, so
/// the computation it waits for can always make progress. The wait uses its own timer thread, so
/// it doesn't depend on the runtime, which can be shutting down.
pub(crate) fn take_trees(
    slot: &watch::Sender<TreeSlot>,
    block: &Arc<Block>,
) -> Option<Arc<PrecomputedTrees>> {
    let is_done = |slot: &TreeSlot| !matches!(slot, TreeSlot::Computing(computing) if Arc::ptr_eq(computing, block));
    let mut receiver = slot.subscribe();
    if !is_done(&receiver.borrow()) {
        let (timeout, timed_out) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(TREE_WAIT_TIMEOUT);
            let _ = timeout.send(());
        });
        let _ = futures::executor::block_on(futures::future::select(
            Box::pin(receiver.wait_for(is_done)),
            timed_out,
        ));
    }

    let mut taken = None;
    slot.send_if_modified(|slot| match slot {
        TreeSlot::Ready(trees) => {
            taken = Arc::ptr_eq(&trees.block, block).then(|| trees.clone());
            *slot = TreeSlot::Empty;
            true
        }
        _ => false,
    });
    taken
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

    /// A commit waits for a running computation of its block, and ignores other blocks.
    #[test]
    fn writer_waits_only_for_its_block() {
        let _init_guard = zebra_test::init();
        let block = orchard_block();
        let slot = watch::Sender::new(TreeSlot::Empty);
        assert!(take_trees(&slot, &block).is_none());

        let guard = TreeSlotGuard::start(slot.clone(), block.clone());
        let other = orchard_block();
        let started = std::time::Instant::now();
        assert!(
            take_trees(&slot, &other).is_none(),
            "another body's commit must not wait",
        );
        assert!(started.elapsed() < TREE_WAIT_TIMEOUT);

        let computing = std::thread::spawn({
            let block = block.clone();
            move || {
                let trees = PrecomputedTrees::compute(block, NoteCommitmentTrees::default())
                    .expect("a block's commitments fit an empty tree");
                guard.finish(Arc::new(trees));
            }
        });
        let trees = take_trees(&slot, &block).expect("the computation finishes");
        assert!(Arc::ptr_eq(&trees.block, &block));
        computing.join().unwrap();
        assert!(
            matches!(*slot.borrow(), TreeSlot::Empty),
            "taking the trees doesn't retain them",
        );

        let abandoned = TreeSlotGuard::start(slot.clone(), other.clone());
        drop(abandoned);
        assert!(
            take_trees(&slot, &other).is_none(),
            "a dropped computation releases waiting commits",
        );
    }

    /// Trees for another block are cleared rather than retained after an unrelated commit.
    #[test]
    fn writer_clears_trees_for_another_block() {
        let _init_guard = zebra_test::init();
        let block = orchard_block();
        let slot = watch::Sender::new(TreeSlot::Empty);
        let guard = TreeSlotGuard::start(slot.clone(), block.clone());
        guard.finish(Arc::new(
            PrecomputedTrees::compute(block, NoteCommitmentTrees::default()).unwrap(),
        ));
        assert!(take_trees(&slot, &orchard_block()).is_none());
        assert!(matches!(*slot.borrow(), TreeSlot::Empty));
    }

    /// A commit stops waiting for a stalled computation and computes the trees itself.
    #[test]
    fn writer_stops_waiting_for_a_stalled_computation() {
        let _init_guard = zebra_test::init();
        let block = orchard_block();
        let slot = watch::Sender::new(TreeSlot::Empty);
        let _stalled = TreeSlotGuard::start(slot.clone(), block.clone());

        let started = std::time::Instant::now();
        assert!(take_trees(&slot, &block).is_none());
        assert!(started.elapsed() >= TREE_WAIT_TIMEOUT);
    }
}
