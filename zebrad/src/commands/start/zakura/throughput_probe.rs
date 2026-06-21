use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::oneshot;
use tracing::warn;

use zebra_chain::{block, serialization::ZcashSerialize};
use zebra_network::zakura::{BlockApplyResult, BlockSyncFrontiers};

/// A debug-only stand-in for block-body verify+commit.
///
/// Bodies still download over Zakura as usual, but instead of running consensus
/// verification and committing to state, the probe advances an in-memory
/// synthetic frontier one contiguous height at a time. It exists to measure the
/// P2P stack's raw block-sync throughput with execution removed; see
/// `P2P_OPTIMIZATION_GOAL.md`.
///
/// The body is *not* re-validated here: the network layer already rejects any
/// body whose hash does not match the announced hash for its height
/// (`zebra_network::zakura` peer routine), and the reorder buffer only releases
/// a contiguous-by-height prefix. So a body reaching the probe is already the
/// expected block at the next height; the probe only confirms contiguity and
/// takes the body's own hash as the new tip.
#[derive(Clone, Debug)]
pub(crate) struct BlocksyncThroughputProbe {
    inner: Arc<Mutex<BlocksyncThroughputProbeState>>,
}

#[derive(Debug)]
struct BlocksyncThroughputProbeState {
    finalized_height: block::Height,
    verified_block_tip: block::Height,
    verified_block_hash: block::Hash,
    target_height: block::Height,
    started: Instant,
    completed_blocks: u64,
    completed_bytes: u64,
    completion_tx: Option<oneshot::Sender<BlocksyncThroughputSummary>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlocksyncThroughputSummary {
    pub(crate) target_height: block::Height,
    pub(crate) verified_block_tip: block::Height,
    pub(crate) completed_blocks: u64,
    pub(crate) completed_bytes: u64,
    pub(crate) elapsed: Duration,
}

impl BlocksyncThroughputProbe {
    pub(crate) fn new(
        initial_frontiers: BlockSyncFrontiers,
        target_height: block::Height,
    ) -> (Self, oneshot::Receiver<BlocksyncThroughputSummary>) {
        let (completion_tx, completion_rx) = oneshot::channel();
        let probe = Self {
            inner: Arc::new(Mutex::new(BlocksyncThroughputProbeState {
                finalized_height: initial_frontiers.finalized_height,
                verified_block_tip: initial_frontiers.verified_block_tip,
                verified_block_hash: initial_frontiers.verified_block_hash,
                target_height,
                started: Instant::now(),
                completed_blocks: 0,
                completed_bytes: 0,
                completion_tx: Some(completion_tx),
            })),
        };
        probe.lock().maybe_complete();
        (probe, completion_rx)
    }

    /// Pretend to apply `block` by advancing the synthetic frontier when it is
    /// the next contiguous height. Returns the apply result and, on a synthetic
    /// commit, the advanced frontier (the driver feeds it back into the reactor
    /// exactly as a real commit's frontier would be).
    pub(crate) fn apply_block(
        &self,
        block: &block::Block,
    ) -> (BlockApplyResult, Option<BlockSyncFrontiers>) {
        let Some(height) = block.coinbase_height() else {
            let hash = block.hash();
            warn!(
                ?hash,
                "Zakura block-sync throughput probe rejected block without coinbase height"
            );
            return (BlockApplyResult::Rejected, None);
        };

        let hash = block.hash();
        let block_bytes = u64::try_from(block.zcash_serialized_size()).unwrap_or(u64::MAX);
        let mut inner = self.lock();

        let expected_height = inner
            .verified_block_tip
            .next()
            .unwrap_or(inner.verified_block_tip);
        if height != expected_height {
            warn!(
                ?height,
                ?expected_height,
                "Zakura block-sync throughput probe rejected non-contiguous block body"
            );
            return (BlockApplyResult::Rejected, None);
        }

        inner.verified_block_tip = height;
        inner.verified_block_hash = hash;
        inner.completed_blocks = inner.completed_blocks.saturating_add(1);
        inner.completed_bytes = inner.completed_bytes.saturating_add(block_bytes);
        let frontiers = inner.frontiers();
        inner.maybe_complete();

        (BlockApplyResult::Committed, Some(frontiers))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BlocksyncThroughputProbeState> {
        self.inner
            .lock()
            .expect("blocksync throughput probe mutex is not poisoned")
    }

    pub(crate) fn verified_tip(&self) -> block::Height {
        self.lock().verified_block_tip
    }

    #[cfg(test)]
    pub(crate) fn synthetic_frontier(&self) -> BlockSyncFrontiers {
        self.lock().frontiers()
    }
}

impl BlocksyncThroughputProbeState {
    fn frontiers(&self) -> BlockSyncFrontiers {
        BlockSyncFrontiers {
            finalized_height: self.finalized_height,
            verified_block_tip: self.verified_block_tip,
            verified_block_hash: self.verified_block_hash,
        }
    }

    fn maybe_complete(&mut self) {
        if self.verified_block_tip < self.target_height {
            return;
        }

        let Some(completion_tx) = self.completion_tx.take() else {
            return;
        };

        let _ = completion_tx.send(BlocksyncThroughputSummary {
            target_height: self.target_height,
            verified_block_tip: self.verified_block_tip,
            completed_blocks: self.completed_blocks,
            completed_bytes: self.completed_bytes,
            elapsed: self.started.elapsed(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use zebra_chain::{block, serialization::ZcashDeserializeInto};
    use zebra_network::zakura::BlockApplyResult;
    use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES};

    use super::*;

    fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
        Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
    }

    #[tokio::test]
    async fn advances_on_contiguous_bodies() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let initial_frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        };

        let (probe, mut completion_rx) =
            BlocksyncThroughputProbe::new(initial_frontiers, block::Height(2));

        // A gap (height 2 before 1) is rejected and leaves the frontier put.
        let (result, frontier) = probe.apply_block(block2.as_ref());
        assert_eq!(result, BlockApplyResult::Rejected);
        assert_eq!(frontier, None);
        assert_eq!(probe.synthetic_frontier(), initial_frontiers);

        let (result, frontier) = probe.apply_block(block1.as_ref());
        assert_eq!(result, BlockApplyResult::Committed);
        assert_eq!(
            frontier.expect("committed body has a frontier"),
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(1),
                verified_block_hash: block1.hash(),
            }
        );

        // Re-applying an already-verified height is non-contiguous, so rejected.
        let (result, _) = probe.apply_block(block1.as_ref());
        assert_eq!(result, BlockApplyResult::Rejected);

        let (result, _) = probe.apply_block(block2.as_ref());
        assert_eq!(result, BlockApplyResult::Committed);
        assert_eq!(
            probe.synthetic_frontier(),
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(2),
                verified_block_hash: block2.hash(),
            }
        );

        let summary = tokio::time::timeout(Duration::from_secs(1), &mut completion_rx)
            .await
            .expect("throughput probe completes at target")
            .expect("completion sender remains live");
        assert_eq!(summary.completed_blocks, 2);
    }

    #[tokio::test]
    async fn completes_when_target_is_current_tip() {
        let initial_frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(1),
            verified_block_tip: block::Height(2),
            verified_block_hash: block::Hash([2; 32]),
        };
        let (probe, mut completion_rx) =
            BlocksyncThroughputProbe::new(initial_frontiers, block::Height(2));

        let summary = tokio::time::timeout(Duration::from_secs(1), &mut completion_rx)
            .await
            .expect("throughput probe completes immediately")
            .expect("completion sender remains live");
        assert_eq!(summary.completed_blocks, 0);
        assert_eq!(summary.completed_bytes, 0);
        assert_eq!(probe.synthetic_frontier(), initial_frontiers);
    }
}
