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

/// Blocks evicted by the fork limit are reported like rejected blocks, so the state service can
/// download and commit them again if the network extends them (#11133).
#[test]
fn evicted_blocks_are_reported_as_rejected() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized, root, _child) = populated_state();
    let received = std::time::Instant::now();

    let max_forks = u8::try_from(crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .expect("the fork limit fits in a u8");
    for i in 1..=max_forks {
        let mut prepared = root
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32])
            .prepare();
        prepared.received_time = Some(received + Duration::from_secs(i.into()));
        state.commit_block(prepared, &finalized).unwrap();
    }

    let (rejected_sender, mut rejected_receiver) = tokio::sync::mpsc::unbounded_channel();
    forget_evicted_blocks(&mut state, &rejected_sender);

    let evicted = rejected_receiver
        .try_recv()
        .expect("one fork was evicted at the fork limit");
    assert!(!state.any_chain_contains(&evicted));
    assert!(
        rejected_receiver.try_recv().is_err(),
        "blocks that are still in another chain are not reported",
    );

    forget_evicted_blocks(&mut state, &rejected_sender);
    assert!(
        rejected_receiver.try_recv().is_err(),
        "blocks are reported once"
    );
}
