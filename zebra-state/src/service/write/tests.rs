//! Regression tests for non-finalized write notifications.

use std::time::Duration;

use zebra_chain::{
    amount::NonNegative, block::Block, chain_tip::ChainTip, parameters::Network,
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    Config,
};

use super::*;

fn populated_state() -> (NonFinalizedState, FinalizedState, Arc<Block>, Arc<Block>) {
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = root.make_fake_child().set_work(10);
    let mut state = NonFinalizedState::new(&network);
    let finalized = FinalizedState::new(&Config::ephemeral(), &network).unwrap();
    finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    state
        .commit_new_chain(root.clone().prepare(), &finalized)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized)
        .unwrap();
    (state, finalized, root, child)
}

#[test]
fn invalidating_only_root_keeps_chain_channels_available() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized, root, child) = populated_state();
    let before = state.clone();
    let (mut tip_sender, latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, state_receiver) = watch::channel(state.clone());

    let result = state.invalidate_block(root.hash());
    update_latest_chain_channels(&state, &finalized.db, &mut tip_sender, &state_sender, None);
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    assert_eq!(latest.best_tip_hash(), Some(child.hash()));
    assert!(state_receiver.borrow().eq_internal_state(&before));
}

#[test]
fn empty_non_finalized_channels_fall_back_to_current_finalized_tip() {
    let _init_guard = zebra_test::init();
    let (finalized, state, genesis) = new_state_with_mainnet_genesis();
    let stale_tip = genesis.block.make_fake_child();
    let (mut tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    tip_sender.set_best_non_finalized_tip(ChainTipBlock::from(stale_tip.prepare()));
    change.last_tip_change().unwrap();
    let (state_sender, state_receiver) = watch::channel(state.clone());

    assert_eq!(
        update_latest_chain_channels(&state, &finalized.db, &mut tip_sender, &state_sender, None),
        Some(Height(0))
    );
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));
    assert!(state_receiver.borrow().is_chain_set_empty());
    let action = change.last_tip_change().unwrap();
    assert!(action.is_reset());
    assert_eq!(action.best_tip_hash(), genesis.hash);

    // Falling back must also allow subsequent finalized-tip notifications.
    let next_finalized = genesis.block.make_fake_child();
    tip_sender.set_finalized_tip(ChainTipBlock::from(next_finalized.clone().prepare()));
    assert_eq!(latest.best_tip_hash(), Some(next_finalized.hash()));
}

#[test]
fn empty_chain_channels_clear_a_stale_tip() {
    let _init_guard = zebra_test::init();
    let (populated, finalized, _root, child) = populated_state();
    let empty = NonFinalizedState::new(&Network::Mainnet);
    let (mut tip_sender, latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    tip_sender.set_best_non_finalized_tip(ChainTipBlock::from(child.prepare()));
    let (state_sender, state_receiver) = watch::channel(populated);

    assert_eq!(
        update_latest_chain_channels(&empty, &finalized.db, &mut tip_sender, &state_sender, None),
        None
    );
    assert_eq!(latest.best_tip_hash(), None);
    assert!(state_receiver.borrow().is_chain_set_empty());
}

#[tokio::test]
async fn write_worker_survives_rejected_root_invalidation() {
    let _init_guard = zebra_test::init();
    let (state, finalized, root, child) = populated_state();
    let before = state.clone();
    let (tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, mut state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: root.hash(),
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(state_receiver.borrow().eq_internal_state(&before));
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(child.hash()));

    // Exercise the same worker again: a non-root invalidation must still succeed.
    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: child.hash(),
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap(), child.hash());
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(root.hash()));

    drop(sender);
    let worker = worker.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            Arc::try_unwrap(worker).unwrap().join().unwrap();
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn write_worker_handles_requests_with_no_non_finalized_chain() {
    let _init_guard = zebra_test::init();
    let (finalized, state, genesis) = new_state_with_mainnet_genesis();
    let (tip_sender, latest, mut change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, mut state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Invalidate {
        hash: genesis.hash,
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(InvalidateError::BlockNotFound(_))));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), change.wait_for_tip_change())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));

    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Reconsider {
        hash: genesis.hash,
        rsp_tx,
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(ReconsiderError::MissingInvalidatedBlock(_))
    ));
    tokio::time::timeout(Duration::from_secs(10), state_receiver.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.best_tip_hash(), Some(genesis.hash));
    assert!(state_receiver.borrow().is_chain_set_empty());

    drop(sender);
    let worker = worker.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            Arc::try_unwrap(worker).unwrap().join().unwrap();
        }),
    )
    .await
    .unwrap()
    .unwrap();
}

#[test]
fn precious_response_follows_tip_and_snapshot_publication() {
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Wake, Waker},
    };

    struct AtResponse {
        tip: LatestChainTip,
        snapshot: watch::Receiver<NonFinalizedState>,
        expected: block::Hash,
        published: AtomicBool,
    }

    impl Wake for AtResponse {
        fn wake(self: Arc<Self>) {
            // Observe the exact send boundary without racing a separate receiver task.
            self.published.store(
                self.tip.best_tip_hash() == Some(self.expected)
                    && self.snapshot.borrow().best_tip().map(|(_, hash)| hash)
                        == Some(self.expected),
                Ordering::SeqCst,
            );
        }
    }

    let _init_guard = zebra_test::init();
    let (mut state, finalized, root, first) = populated_state();
    let second = root
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([1; 32]);
    let mut prepared = second.clone().prepare();
    prepared.received_time = Some(std::time::Instant::now());
    state.commit_block(prepared, &finalized.db).unwrap();
    assert_eq!(state.best_tip().unwrap().1, first.hash());
    let (mut chain_tip_sender, tip, _) = ChainTipSender::new(None, &Network::Mainnet);
    let (non_finalized_state_sender, snapshot) = watch::channel(state.clone());
    update_latest_chain_channels(
        &state,
        &finalized.db,
        &mut chain_tip_sender,
        &non_finalized_state_sender,
        None,
    );
    let observation = Arc::new(AtResponse {
        tip: tip.clone(),
        snapshot,
        expected: second.hash(),
        published: AtomicBool::new(false),
    });
    let waker = Waker::from(observation.clone());
    let mut cx = Context::from_waker(&waker);
    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    assert!(Pin::new(&mut rsp_rx).poll(&mut cx).is_pending());
    let (queued, non_finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    queued
        .send(NonFinalizedWriteMessage::Precious {
            hash: second.hash(),
            rsp_tx,
        })
        .unwrap();
    drop(queued);
    let (invalid_block_reset_sender, _resets) = tokio::sync::mpsc::unbounded_channel();
    // Bind the worker first: temporaries in a single `.run()` statement, such as the unused
    // finalized-block sender, would live until `run()` returns and keep its first loop waiting.
    let worker = WriteBlockWorkerTask {
        finalized_block_write_receiver: tokio::sync::mpsc::unbounded_channel().1,
        non_finalized_block_write_receiver,
        finalized_state: finalized,
        non_finalized_state: state,
        invalid_block_reset_sender,
        non_finalized_rejected_sender: tokio::sync::mpsc::unbounded_channel().0,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path: None,
    };
    worker.run();
    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), second.hash());
    assert!(
        observation.published.load(Ordering::SeqCst),
        "the selected tip and read-state snapshot must be published before the response",
    );
}
