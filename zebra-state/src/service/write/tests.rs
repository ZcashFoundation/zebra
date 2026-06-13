//! Tests for the any-order block write worker, driven through the
//! [`WriteMessage`] channel.

use std::{sync::Arc, time::Duration};

use tokio::sync::{mpsc::UnboundedSender, oneshot, watch};

use zebra_chain::{
    block::{Block, Hash, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{
    error::CommitCheckpointVerifiedError,
    service::{
        finalized_state::FinalizedState, non_finalized_state::NonFinalizedState, ChainTipSender,
    },
    CheckpointVerifiedBlock, Config,
};

use super::{BlockWriteSender, WriteMessage};

/// Returns `true` if `error` is the `OutOfOrder` checkpoint commit error.
///
/// Matched on the Debug output because `CommitCheckpointVerifiedError` wraps
/// `CommitBlockError` in a private field.
fn is_out_of_order(error: &CommitCheckpointVerifiedError) -> bool {
    format!("{error:?}").contains("OutOfOrder")
}

/// Returns the first `count` mainnet blocks (genesis onward).
fn mainnet_blocks(count: u32) -> Vec<Arc<Block>> {
    let blocks = Network::Mainnet.blockchain_map();
    (0..count)
        .map(|height| {
            blocks
                .get(&height)
                .expect("test vectors include the first blocks of mainnet")
                .zcash_deserialize_into()
                .expect("test vectors deserialize")
        })
        .collect()
}

/// A spawned write worker over a fresh ephemeral database, with handles needed
/// to drive and observe it.
struct WorkerHarness {
    sender: UnboundedSender<WriteMessage>,
    read_db: crate::service::finalized_state::ZebraDb,
    // Kept alive so the worker doesn't see channel closes and shut down.
    _reset_rx: tokio::sync::mpsc::UnboundedReceiver<Hash>,
    _rejected_rx: tokio::sync::mpsc::UnboundedReceiver<Hash>,
    _nfs_rx: watch::Receiver<NonFinalizedState>,
    _task: Option<Arc<std::thread::JoinHandle<()>>>,
}

impl WorkerHarness {
    fn new() -> Self {
        let network = Network::Mainnet;
        let finalized_state = FinalizedState::new(
            &Config::ephemeral(),
            &network,
            #[cfg(feature = "elasticsearch")]
            false,
        );
        let read_db = finalized_state.db.clone();
        let non_finalized_state = NonFinalizedState::new(&network);
        let (chain_tip_sender, _latest, _change) = ChainTipSender::new(None, &network);
        let (nfs_sender, nfs_rx) = watch::channel(non_finalized_state.clone());

        let (block_write_sender, reset_rx, rejected_rx, task) = BlockWriteSender::spawn(
            finalized_state,
            non_finalized_state,
            chain_tip_sender,
            nfs_sender,
            None,
        );

        Self {
            sender: block_write_sender
                .sender
                .expect("spawn always returns a sender"),
            read_db,
            _reset_rx: reset_rx,
            _rejected_rx: rejected_rx,
            _nfs_rx: nfs_rx,
            _task: task,
        }
    }

    /// Sends a checkpoint block and returns its commit result.
    #[allow(clippy::unwrap_in_result)]
    fn send_checkpoint(&self, block: Arc<Block>) -> Result<Hash, CommitCheckpointVerifiedError> {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        self.sender
            .send(WriteMessage::Checkpoint((
                CheckpointVerifiedBlock::from(block),
                rsp_tx,
            )))
            .expect("worker is alive");
        rsp_rx.blocking_recv().expect("worker sent a response")
    }

    /// Waits for the database finalized tip to reach `height`.
    fn wait_for_durable(&self, height: Height) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.read_db.finalized_tip_height() < Some(height) {
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for height {height:?} to be durable",
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// A run of in-order checkpoint blocks commits and reaches the database.
#[test]
fn checkpoint_blocks_commit_in_order() {
    let _init_guard = zebra_test::init();

    let harness = WorkerHarness::new();
    let blocks = mainnet_blocks(4);

    for block in &blocks {
        let expected = block.hash();
        let hash = harness
            .send_checkpoint(block.clone())
            .expect("in-order checkpoint blocks commit");
        assert_eq!(hash, expected);
    }

    harness.wait_for_durable(Height(3));
}

/// A checkpoint block that doesn't extend the write frontier is rejected with
/// `OutOfOrder`, and the worker keeps accepting in-order blocks afterward.
#[test]
fn checkpoint_block_ahead_of_frontier_is_out_of_order() {
    let _init_guard = zebra_test::init();

    let harness = WorkerHarness::new();
    let blocks = mainnet_blocks(4);

    // Commit genesis and block 1.
    harness
        .send_checkpoint(blocks[0].clone())
        .expect("genesis commits");
    harness
        .send_checkpoint(blocks[1].clone())
        .expect("block 1 commits");

    // Block 3 is ahead of the frontier (block 2 is missing): OutOfOrder.
    let error = harness
        .send_checkpoint(blocks[3].clone())
        .expect_err("a block ahead of the frontier is rejected");
    assert!(
        is_out_of_order(&error),
        "expected OutOfOrder, got {error:?}",
    );

    // The worker is unwedged: block 2 then block 3 commit in order.
    harness
        .send_checkpoint(blocks[2].clone())
        .expect("block 2 commits after the gap");
    harness
        .send_checkpoint(blocks[3].clone())
        .expect("block 3 commits after its parent");

    harness.wait_for_durable(Height(3));
}

/// Re-sending an already-committed checkpoint block (a stale duplicate, whose
/// parent is below the frontier) is rejected with `OutOfOrder` rather than
/// wedging the worker.
#[test]
fn restale_checkpoint_block_is_out_of_order() {
    let _init_guard = zebra_test::init();

    let harness = WorkerHarness::new();
    let blocks = mainnet_blocks(3);

    harness
        .send_checkpoint(blocks[0].clone())
        .expect("genesis commits");
    harness
        .send_checkpoint(blocks[1].clone())
        .expect("block 1 commits");
    harness
        .send_checkpoint(blocks[2].clone())
        .expect("block 2 commits");

    // Re-send block 1: its parent (genesis) is below the frontier, and block 1
    // is not a chain tip, so it is OutOfOrder, not a wedge.
    let error = harness
        .send_checkpoint(blocks[1].clone())
        .expect_err("a re-sent committed block is rejected");
    assert!(
        is_out_of_order(&error),
        "expected OutOfOrder, got {error:?}",
    );

    // The worker still accepts the next in-order block.
    let blocks = mainnet_blocks(4);
    harness
        .send_checkpoint(blocks[3].clone())
        .expect("block 3 commits after the re-stale rejection");

    harness.wait_for_durable(Height(3));
}
