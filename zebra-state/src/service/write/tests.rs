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

/// Finalizing re-inserts the remaining chains under the fork limits, so it can evict forks. Those
/// blocks are reported like rejected blocks, and the non-finalized state without them is
/// published, before the next block is committed.
#[test]
fn forks_evicted_by_finalizing_are_reported_and_published() {
    let _init_guard = zebra_test::init();
    let (mut finalized, mut state, genesis) = new_state_with_mainnet_genesis();
    finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let mut best = vec![genesis.block.make_fake_child().set_work(10)];
    state
        .commit_new_chain(best[0].clone().prepare(), &finalized.db)
        .unwrap();
    let min_blocks = usize::try_from(crate::constants::MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT)
        .expect("the minimum fits in a usize");
    while best.len() < min_blocks + 2 {
        let tip = best.last().unwrap().make_fake_child().set_work(10);
        state
            .commit_block(tip.clone().prepare(), &finalized.db)
            .unwrap();
        best.push(tip);
    }
    let forks: Vec<_> = [1, 2]
        .map(|i| best[2].make_fake_child().set_block_commitment([i; 32]))
        .into_iter()
        .collect();
    for fork in &forks {
        state
            .commit_block(fork.clone().prepare(), &finalized.db)
            .unwrap();
    }
    assert_eq!(state.chain_count(), 3);
    assert!(state.take_evicted_block_hashes().is_empty());

    state.set_max_index_entries(Some(1));
    let (mut tip_sender, _latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let (rejected_sender, mut rejected_receiver) = tokio::sync::mpsc::unbounded_channel();

    finalize_root_blocks_and_forget_evicted(
        &mut finalized,
        &mut state,
        &mut None,
        &rejected_sender,
        &mut tip_sender,
        &state_sender,
        None,
    );

    assert!(finalized.db.finalized_tip_height() > Some(Height(0)));
    let evicted = rejected_receiver
        .try_recv()
        .expect("finalizing evicted one fork");
    assert!(forks.iter().any(|fork| fork.hash() == evicted));
    assert!(!state.any_chain_contains(&evicted));
    assert!(rejected_receiver.try_recv().is_err());

    assert!(state_receiver.has_changed().unwrap());
    assert!(!state_receiver.borrow().any_chain_contains(&evicted));
}

/// Finalizing re-inserts side chains one at a time, so a fork can be evicted before a later
/// fork that shares its side-branch ancestor is back. That ancestor must not be reported.
#[test]
fn finalizing_does_not_report_ancestors_of_retained_forks() {
    let _init_guard = zebra_test::init();
    let (mut finalized, mut state, genesis) = new_state_with_mainnet_genesis();
    finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let mut best = vec![genesis.block.make_fake_child().set_work(10)];
    state
        .commit_new_chain(best[0].clone().prepare(), &finalized.db)
        .unwrap();
    let min_blocks = usize::try_from(crate::constants::MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT)
        .expect("the minimum fits in a usize");
    while best.len() < min_blocks + 2 {
        let tip = best.last().unwrap().make_fake_child().set_work(10);
        state
            .commit_block(tip.clone().prepare(), &finalized.db)
            .unwrap();
        best.push(tip);
    }

    // Side chains are re-inserted from the most work down: `high` first, then `middle`, which
    // evicts `high`, then `low`, which shares `shared` with `high` and evicts `middle`.
    let shared = best[2]
        .make_fake_child()
        .set_block_commitment([1; 32])
        .set_work(1);
    let high = shared
        .make_fake_child()
        .set_block_commitment([2; 32])
        .set_work(5);
    let low = shared
        .make_fake_child()
        .set_block_commitment([3; 32])
        .set_work(1);
    let middle = best[2]
        .make_fake_child()
        .set_block_commitment([4; 32])
        .set_work(3);
    for block in [&shared, &high, &low, &middle] {
        state
            .commit_block(block.clone().prepare(), &finalized.db)
            .unwrap();
    }
    assert_eq!(state.chain_count(), 4);
    assert!(state.take_evicted_block_hashes().is_empty());

    state.set_max_index_entries(Some(1));
    let (mut tip_sender, _latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, _state_receiver) = watch::channel(state.clone());
    let (rejected_sender, mut rejected_receiver) = tokio::sync::mpsc::unbounded_channel();

    finalize_root_blocks_and_forget_evicted(
        &mut finalized,
        &mut state,
        &mut None,
        &rejected_sender,
        &mut tip_sender,
        &state_sender,
        None,
    );

    assert!(state.any_chain_contains(&shared.hash()));
    assert!(state.any_chain_contains(&low.hash()));
    let mut rejected = std::collections::HashSet::new();
    while let Ok(hash) = rejected_receiver.try_recv() {
        assert!(rejected.insert(hash), "each evicted block is reported once");
    }
    assert_eq!(rejected, [high.hash(), middle.hash()].into());
}
