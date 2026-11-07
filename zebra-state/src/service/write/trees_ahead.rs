//! Note commitment tree updates computed ahead of the checkpoint block writer.
//!
//! A block's note commitment trees depend only on its parent's trees and its own note
//! commitments, so a worker thread can update them for queued blocks while the writer commits
//! earlier ones. The writer decides which chain of blocks the worker follows: every job either
//! carries the trees to start from, or continues from the previous job's result.

use std::{
    collections::VecDeque,
    sync::{mpsc, Arc},
    thread,
    time::Instant,
};

use zebra_chain::{
    block::{self, Block},
    parallel::tree::{NoteCommitmentTreeError, NoteCommitmentTrees},
};

use crate::service::queued_blocks::QueuedCheckpointVerified;

/// The maximum number of queued blocks whose trees are computed ahead of the writer.
pub(crate) const TREES_AHEAD_LIMIT: usize = 256;

/// A block's updated note commitment trees, or the error from updating them.
pub(crate) type TreesResult = Result<NoteCommitmentTrees, NoteCommitmentTreeError>;

struct Job {
    block: Arc<Block>,
    /// The parent's trees, or `None` to continue from the previous job's result.
    base: Option<NoteCommitmentTrees>,
    /// Receives `None` if there were no trees to continue from.
    reply: mpsc::SyncSender<Option<TreesResult>>,
}

/// A queued block, and the receiver for its precomputed trees, if they were requested.
pub(crate) struct PendingBlock {
    queued: QueuedCheckpointVerified,
    trees: Option<mpsc::Receiver<Option<TreesResult>>>,
}

impl PendingBlock {
    /// Returns the queued block, and a handle for its precomputed trees.
    pub(crate) fn into_parts(self) -> (QueuedCheckpointVerified, PendingTrees) {
        (self.queued, PendingTrees(self.trees))
    }
}

/// A queued block's precomputed trees, if they were requested.
pub(crate) struct PendingTrees(Option<mpsc::Receiver<Option<TreesResult>>>);

impl PendingTrees {
    /// Waits for the trees, if they were requested and the worker had trees to update.
    pub(crate) fn wait(self) -> Option<TreesResult> {
        self.0?.recv().ok().flatten()
    }
}

/// Queued checkpoint blocks, with tree updates running ahead on a worker thread.
pub(crate) struct TreesAhead {
    jobs: Option<mpsc::Sender<Job>>,
    pending: VecDeque<PendingBlock>,
    /// The hash of the last block sent to the worker, if the next pending block can continue
    /// from its trees.
    last_submitted: Option<block::Hash>,
}

impl TreesAhead {
    /// Starts the worker thread, which exits when this queue is dropped.
    ///
    /// No trees are computed ahead until [`Self::resume_from`] gives the worker trees to start from.
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let span = tracing::Span::current();
        let jobs = thread::Builder::new()
            .name("checkpoint-trees".to_string())
            .spawn(move || span.in_scope(|| run_worker(receiver)))
            .ok()
            .map(|_handle| sender);

        Self {
            jobs,
            pending: VecDeque::new(),
            last_submitted: None,
        }
    }

    /// Returns true if no blocks are queued.
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns true if `TREES_AHEAD_LIMIT` blocks are queued.
    pub(crate) fn is_full(&self) -> bool {
        self.pending.len() >= TREES_AHEAD_LIMIT
    }

    /// Queues a block, and computes its trees ahead if it continues the chain of submitted blocks.
    pub(crate) fn push(&mut self, queued: QueuedCheckpointVerified) {
        let parent_hash = queued.0.block.header.previous_block_hash;
        let trees = if self.last_submitted == Some(parent_hash) {
            self.submit_block(queued.0.block.clone(), queued.0.hash, None)
        } else {
            self.last_submitted = None;
            None
        };
        self.pending.push_back(PendingBlock { queued, trees });
    }

    /// Removes the next block to commit.
    pub(crate) fn pop(&mut self) -> Option<PendingBlock> {
        self.pending.pop_front()
    }

    /// Starts computing trees for the queued blocks that descend from `tip_hash`, whose trees are
    /// `tip_trees`, if they aren't already being computed.
    ///
    /// Does nothing while the worker is already following a chain of queued blocks. The writer
    /// calls this after its commits, so trees are computed ahead again after a block whose trees
    /// weren't, such as the first block or a block after a failed commit.
    pub(crate) fn resume_from(&mut self, tip_hash: block::Hash, tip_trees: &NoteCommitmentTrees) {
        if self.jobs.is_none() || self.last_submitted.is_some() {
            return;
        }

        let mut parent = tip_hash;
        let mut base = Some(tip_trees.clone());
        let mut jobs = Vec::new();
        for (index, pending) in self.pending.iter().enumerate() {
            if pending.queued.0.block.header.previous_block_hash != parent
                || pending.trees.is_some()
            {
                break;
            }
            jobs.push((index, base.take()));
            parent = pending.queued.0.hash;
        }

        for (index, base) in jobs {
            let queued_block = self.pending[index].queued.0.block.clone();
            let hash = self.pending[index].queued.0.hash;
            let trees = self.submit_block(queued_block, hash, base);
            self.pending[index].trees = trees;
        }
    }

    /// Discards the trees computed for queued blocks, because a commit failed and they may
    /// descend from the failed block.
    pub(crate) fn discard(&mut self) {
        for pending in &mut self.pending {
            pending.trees = None;
        }
        self.last_submitted = None;
    }

    fn submit_block(
        &mut self,
        block: Arc<Block>,
        hash: block::Hash,
        base: Option<NoteCommitmentTrees>,
    ) -> Option<mpsc::Receiver<Option<TreesResult>>> {
        let jobs = self.jobs.as_ref()?;
        let (reply, receiver) = mpsc::sync_channel(1);
        if jobs.send(Job { block, base, reply }).is_err() {
            // The worker stopped, so the writer computes every block's trees itself.
            self.jobs = None;
            self.last_submitted = None;
            return None;
        }
        self.last_submitted = Some(hash);
        Some(receiver)
    }
}

fn run_worker(jobs: mpsc::Receiver<Job>) {
    let mut trees: Option<NoteCommitmentTrees> = None;

    while let Ok(Job { block, base, reply }) = jobs.recv() {
        if base.is_some() {
            trees = base;
        }

        let result = trees.as_ref().cloned().map(|mut next| {
            let start = Instant::now();
            let result = next.update_trees_parallel(&block).map(|()| next);
            metrics::histogram!(
                "state.block_writer.phase.duration_seconds",
                "phase" => "checkpoint_trees_ahead"
            )
            .record(start.elapsed().as_secs_f64());
            result
        });

        // A failed update leaves nothing to continue from until the writer sends new trees.
        trees = match &result {
            Some(Ok(next)) => Some(next.clone()),
            _ => None,
        };

        // The writer may have dropped this block.
        let _ = reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use proptest::prelude::*;
    use tokio::sync::oneshot;

    use super::*;
    use crate::{service::arbitrary::PreparedChain, CheckpointVerifiedBlock};

    fn queued(block: &Arc<Block>) -> QueuedCheckpointVerified {
        let (rsp_tx, _rsp_rx) = oneshot::channel();
        (CheckpointVerifiedBlock::from(block.clone()), rsp_tx, None)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)))]

        /// Trees computed ahead match sequential updates, nothing is computed before the first
        /// `resume_from` or after `discard`, and computing restarts after the next `resume_from`.
        #[test]
        fn trees_ahead_match_sequential_updates(
            (chain, _count, _network, _history_tree) in PreparedChain::default().no_shrink()
        ) {
            let _init_guard = zebra_test::init();

            // The trees after each block, updated one block at a time.
            let mut expected = Vec::new();
            let mut trees = NoteCommitmentTrees::default();
            for block in chain.iter() {
                trees.update_trees_parallel(&block.block).expect("valid chain");
                expected.push(trees.clone());
            }

            let mut queue = TreesAhead::new();
            for block in chain.iter() {
                queue.push(queued(&block.block));
            }

            // The writer commits the first block itself, then resumes from its trees.
            let (_, first) = queue.pop().expect("chain has blocks").into_parts();
            prop_assert!(first.wait().is_none(), "nothing is computed before resume_from");
            queue.resume_from(chain[0].hash, &expected[0]);

            let discard_at = chain.len() / 2;
            for index in 1..chain.len() {
                let (_, trees) = queue.pop().expect("one queued block per chain block").into_parts();

                if index == discard_at {
                    // A failed commit here: this block and later ones were queued before the
                    // discard, and their results aren't used.
                    queue.discard();
                    drop(trees);
                    continue;
                }

                let trees = trees.wait();
                if index == discard_at + 1 {
                    // The writer commits this block itself after a failure, then resumes.
                    prop_assert!(trees.is_none(), "discarded results are not returned");
                    queue.resume_from(chain[index].hash, &expected[index]);
                } else {
                    let trees = trees.expect("computed ahead").expect("valid chain");
                    prop_assert_eq!(trees, expected[index].clone());
                }
            }
            prop_assert!(queue.is_empty());
        }
    }
}
