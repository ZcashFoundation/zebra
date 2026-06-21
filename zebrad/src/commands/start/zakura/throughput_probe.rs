use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::oneshot;
use tracing::warn;

use zebra_chain::{block, serialization::ZcashSerialize};
use zebra_network::zakura::{
    BlockApplyResult, BlockSyncBlockMeta, BlockSyncEvent, BlockSyncFrontiers, BlockSyncHandle,
};

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
    expected_hashes: HashMap<block::Height, block::Hash>,
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

#[derive(Clone, Debug)]
pub(crate) enum BlockSyncApplyMode {
    Normal,
    ThroughputProbe(BlocksyncThroughputProbe),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct BlocksyncThroughputProbeApply {
    pub(super) result: BlockApplyResult,
    pub(super) local_frontier: Option<BlockSyncFrontiers>,
}

impl BlockSyncApplyMode {
    pub(super) fn record_expected_blocks(&self, blocks: &[BlockSyncBlockMeta]) {
        if let Self::ThroughputProbe(probe) = self {
            probe.record_expected_blocks(blocks);
        }
    }

    pub(super) fn apply_probe(
        &self,
        block: &block::Block,
    ) -> Option<BlocksyncThroughputProbeApply> {
        match self {
            Self::Normal => None,
            Self::ThroughputProbe(probe) => Some(probe.apply_block(block)),
        }
    }

    pub(super) fn is_throughput_probe(&self) -> bool {
        matches!(self, Self::ThroughputProbe(_))
    }

    pub(super) fn send_synthetic_chain_tip_grow(
        &self,
        block_sync: &BlockSyncHandle,
        result: BlockApplyResult,
        local_frontier: Option<BlockSyncFrontiers>,
    ) {
        if result != BlockApplyResult::Committed || !self.is_throughput_probe() {
            return;
        }

        if let Some(frontiers) = local_frontier {
            let _ = block_sync.send_control(BlockSyncEvent::ChainTipGrow(frontiers));
        }
    }
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
                expected_hashes: HashMap::new(),
                started: Instant::now(),
                completed_blocks: 0,
                completed_bytes: 0,
                completion_tx: Some(completion_tx),
            })),
        };
        probe.maybe_complete();
        (probe, completion_rx)
    }

    fn record_expected_blocks(&self, blocks: &[BlockSyncBlockMeta]) {
        let mut inner = self
            .inner
            .lock()
            .expect("blocksync throughput probe mutex is not poisoned");
        inner
            .expected_hashes
            .extend(blocks.iter().map(|meta| (meta.height, meta.hash)));
    }

    fn apply_block(&self, block: &block::Block) -> BlocksyncThroughputProbeApply {
        let Some(height) = block.coinbase_height() else {
            let hash = block.hash();
            warn!(
                ?hash,
                "Zakura block-sync throughput probe rejected block without coinbase height"
            );
            return BlocksyncThroughputProbeApply {
                result: BlockApplyResult::Rejected,
                local_frontier: None,
            };
        };

        let hash = block.hash();
        let block_bytes = u64::try_from(block.zcash_serialized_size()).unwrap_or(u64::MAX);
        let mut inner = self
            .inner
            .lock()
            .expect("blocksync throughput probe mutex is not poisoned");

        let expected_height = inner
            .verified_block_tip
            .next()
            .unwrap_or(inner.verified_block_tip);
        let expected_hash = inner.expected_hashes.get(&height).copied();

        let result = if height != expected_height {
            warn!(
                ?height,
                ?expected_height,
                "Zakura block-sync throughput probe rejected non-contiguous block body"
            );
            BlockApplyResult::Rejected
        } else if expected_hash != Some(hash) {
            warn!(
                ?height,
                ?hash,
                ?expected_hash,
                "Zakura block-sync throughput probe rejected body with unexpected hash"
            );
            BlockApplyResult::Rejected
        } else {
            inner.verified_block_tip = height;
            inner.verified_block_hash = hash;
            inner.expected_hashes.remove(&height);
            inner.completed_blocks = inner.completed_blocks.saturating_add(1);
            inner.completed_bytes = inner.completed_bytes.saturating_add(block_bytes);
            BlockApplyResult::Committed
        };

        let local_frontier =
            (result == BlockApplyResult::Committed).then_some(BlockSyncFrontiers {
                finalized_height: inner.finalized_height,
                verified_block_tip: inner.verified_block_tip,
                verified_block_hash: inner.verified_block_hash,
            });

        if result == BlockApplyResult::Committed {
            inner.maybe_complete();
        }

        BlocksyncThroughputProbeApply {
            result,
            local_frontier,
        }
    }

    fn maybe_complete(&self) {
        let mut inner = self
            .inner
            .lock()
            .expect("blocksync throughput probe mutex is not poisoned");
        inner.maybe_complete();
    }

    #[cfg(test)]
    pub(crate) fn synthetic_frontier(&self) -> BlockSyncFrontiers {
        let inner = self
            .inner
            .lock()
            .expect("blocksync throughput probe mutex is not poisoned");
        BlockSyncFrontiers {
            finalized_height: inner.finalized_height,
            verified_block_tip: inner.verified_block_tip,
            verified_block_hash: inner.verified_block_hash,
        }
    }
}

impl BlocksyncThroughputProbeState {
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
    use zebra_network::zakura::{BlockApplyResult, BlockSizeEstimate, BlockSyncBlockMeta};
    use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES};

    use super::*;

    fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
        Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
    }

    #[tokio::test]
    async fn validates_contiguous_expected_bodies() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let initial_frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        };

        let (probe, mut completion_rx) =
            BlocksyncThroughputProbe::new(initial_frontiers, block::Height(2));
        probe.record_expected_blocks(&[
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block1.hash(),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block2.hash(),
                size: BlockSizeEstimate::Unknown,
            },
        ]);

        let out_of_order = probe.apply_block(block2.as_ref());
        assert_eq!(out_of_order.result, BlockApplyResult::Rejected);
        assert_eq!(probe.synthetic_frontier(), initial_frontiers);

        let accepted = probe.apply_block(block1.as_ref());
        assert_eq!(accepted.result, BlockApplyResult::Committed);
        assert_eq!(
            accepted.local_frontier.expect("accepted body has frontier"),
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(1),
                verified_block_hash: block1.hash(),
            }
        );

        let repeated = probe.apply_block(block1.as_ref());
        assert_eq!(repeated.result, BlockApplyResult::Rejected);

        let accepted = probe.apply_block(block2.as_ref());
        assert_eq!(accepted.result, BlockApplyResult::Committed);
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

        let (mismatch_probe, _completion_rx) =
            BlocksyncThroughputProbe::new(initial_frontiers, block::Height(1));
        mismatch_probe.record_expected_blocks(&[BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block::Hash([9; 32]),
            size: BlockSizeEstimate::Unknown,
        }]);
        let mismatch = mismatch_probe.apply_block(block1.as_ref());
        assert_eq!(mismatch.result, BlockApplyResult::Rejected);
        assert_eq!(mismatch_probe.synthetic_frontier(), initial_frontiers);
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
