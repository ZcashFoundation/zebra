//! Regression tests for non-finalized write notifications.

use std::{sync::atomic::Ordering, time::Duration};

use zebra_chain::{
    amount::NonNegative, block::Block, chain_tip::ChainTip, parameters::Network,
    serialization::ZcashDeserializeInto, value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    service::{
        arbitrary::{early_mining_work_counts, EarlyMiningWorkCounts},
        precomputed_trees::REUSED_TREES,
    },
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    CommitSemanticallyVerifiedError, Config, ContextuallyVerifiedBlock,
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

/// Closes the block write channels and waits for the block write thread to exit.
///
/// The thread shuts down the database when its channels close. If a test returns without waiting
/// for that, the process can exit while RocksDB is still flushing on that thread, and RocksDB
/// aborts with `pthread lock: Invalid argument` when its static state is destroyed.
async fn join_write_worker(
    sender: BlockWriteSender,
    worker: Option<Arc<std::thread::JoinHandle<()>>>,
) {
    drop(sender);
    let worker = worker.expect("the block write thread was spawned");
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            Arc::try_unwrap(worker)
                .expect("the test holds the only handle to the block write thread")
                .join()
                .expect("the block write thread does not panic");
        }),
    )
    .await
    .expect("the block write thread exits once its channels close")
    .expect("the join task does not panic");
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

    join_write_worker(sender, worker).await;
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

    join_write_worker(sender, worker).await;
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

    let NonFinalizedWriteOutcome::Rejected(evicted) = rejected_receiver
        .try_recv()
        .expect("one fork was evicted at the fork limit")
    else {
        panic!("evicted blocks are reported as rejected");
    };
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
    let NonFinalizedWriteOutcome::Rejected(evicted) = rejected_receiver
        .try_recv()
        .expect("finalizing evicted one fork")
    else {
        panic!("evicted blocks are reported as rejected");
    };
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
    while let Ok(outcome) = rejected_receiver.try_recv() {
        let NonFinalizedWriteOutcome::Rejected(hash) = outcome else {
            panic!("evicted blocks are reported as rejected");
        };
        assert!(rejected.insert(hash), "each evicted block is reported once");
    }
    assert_eq!(rejected, [high.hash(), middle.hash()].into());
}

/// The block write task computes early mining work on a block that extends the best tip, and
/// prepares that block's commit with it. Then the block's commit adds the prepared chain.
#[tokio::test]
async fn write_worker_computes_work_for_a_mining_candidate() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let [mut block1, block2]: [Block; 2] = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
    ]
    .map(|bytes| bytes.zcash_deserialize_into().expect("block is valid"));

    // Mainnet block 1 was mined hours after the genesis block, which the contextual time rule
    // doesn't allow. The state doesn't check proof of work, so the header can be changed.
    let context =
        read::difficulty::get_block_template_chain_info(&state, &finalized.db, &Network::Mainnet)
            .expect("a state with a genesis block has a context for the next block");
    let header = Arc::make_mut(&mut block1.header);
    header.time = context.min_time.into();
    header.difficulty_threshold = context.expected_difficulty;
    let (block1, block2) = (Arc::new(block1), Arc::new(block2));

    let (tip_sender, latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, _state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    // The block verifier keeps this guard until it is done with the block.
    let (_guard, verification) = watch::channel(());
    let reused_trees = || REUSED_TREES.load(Ordering::SeqCst);
    let prepared_commits = || PREPARED_COMMITS.load(Ordering::SeqCst);
    let (reused_trees_before, prepared_commits_before) = (reused_trees(), prepared_commits());
    let prepares_before = PREPARES_RUN.load(Ordering::SeqCst);

    // A block that doesn't extend the best tip doesn't get any work.
    assert_eq!(
        send_mining_candidate(tx, &block2.prepare(), verification.clone()).await,
        None
    );

    let commit = block1.clone().prepare();
    let chain_info = send_mining_candidate(tx, &commit, verification)
        .await
        .expect("a block that extends the best tip gets work");
    assert_eq!(chain_info.tip_hash, block1.hash());
    assert_eq!(chain_info.tip_height, Height(1));
    wait_for_prepares(tx, prepares_before + 1).await;

    // The prepare and the commit only use the candidate's work for the block and hash list
    // objects the candidate was sent with, so this fails if the messages stop sharing either.
    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Commit((commit, rsp_tx, None)))
        .unwrap();
    let committed = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .expect("the write task answers")
        .expect("the write task is running");
    assert_eq!(committed.ok(), Some(block1.hash()));
    assert_eq!(latest.best_tip_hash(), Some(block1.hash()));
    assert_eq!(
        reused_trees(),
        reused_trees_before + 1,
        "the prepare reuses the trees that were computed for the candidate"
    );
    assert_eq!(
        prepared_commits(),
        prepared_commits_before + 1,
        "the commit adds the prepared chain"
    );
    assert_eq!(
        PREPARES_RUN.load(Ordering::SeqCst),
        prepares_before + 1,
        "only a candidate with work prepares its block's commit",
    );

    join_write_worker(sender, worker).await;
}

/// Waits until the block write task that `tx` sends to has run `prepares` prepares since the
/// process started. Panics if the task stops first.
async fn wait_for_prepares(tx: &UnboundedSender<NonFinalizedWriteMessage>, prepares: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while PREPARES_RUN.load(Ordering::SeqCst) < prepares {
            assert!(!tx.is_closed(), "the block write task stopped");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the block write task prepares the commit");
}

/// Returns Mainnet block 1, with a header that is contextually valid on the genesis block in
/// `state` and `db`.
fn block1_on_genesis(state: &NonFinalizedState, db: &ZebraDb) -> Arc<Block> {
    let mut block1: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("block is valid");

    // Mainnet block 1 was mined hours after the genesis block, which the contextual time rule
    // doesn't allow. The state doesn't check proof of work, so the header can be changed.
    let context = read::difficulty::get_block_template_chain_info(state, db, &Network::Mainnet)
        .expect("a state with a genesis block has a context for the next block");
    let header = Arc::make_mut(&mut block1.header);
    header.time = context.min_time.into();
    header.difficulty_threshold = context.expected_difficulty;

    Arc::new(block1)
}

/// Returns a copy of `block` with another `nonce`, so it has the same parent and transactions,
/// and another hash.
fn nonce_variant(block: &Block, nonce: [u8; 32]) -> Arc<Block> {
    let mut variant = block.clone();
    Arc::make_mut(&mut variant.header).nonce = nonce.into();

    Arc::new(variant)
}

/// Sends `block`'s block and transaction hashes to the block write task as a mining candidate,
/// and returns the answer.
///
/// `verification` is the receiver for the block verifier's guard. The block write task only
/// prepares the block's commit while the guard is open.
async fn send_mining_candidate(
    tx: &UnboundedSender<NonFinalizedWriteMessage>,
    block: &SemanticallyVerifiedBlock,
    verification: watch::Receiver<()>,
) -> Option<GetBlockTemplateChainInfo> {
    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::MiningCandidate {
        block: block.block.clone(),
        transaction_hashes: block.transaction_hashes.clone(),
        verification,
        rsp_tx,
    })
    .unwrap();

    tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .expect("the write task answers")
        .expect("the write task is running")
}

/// Sends `block` to the block write task to be committed, with `contextual`, the result of
/// validating it as a block proposal if it solves one. Returns the response.
async fn send_commit(
    tx: &UnboundedSender<NonFinalizedWriteMessage>,
    block: SemanticallyVerifiedBlock,
    contextual: Option<Box<ContextuallyVerifiedBlock>>,
) -> Result<block::Hash, CommitSemanticallyVerifiedError> {
    let (rsp_tx, rsp_rx) = oneshot::channel();
    tx.send(NonFinalizedWriteMessage::Commit((
        block, rsp_tx, contextual,
    )))
    .unwrap();

    tokio::time::timeout(Duration::from_secs(10), rsp_rx)
        .await
        .expect("the write task answers")
        .expect("the write task is running")
}

/// A mining candidate's work is kept when another block is committed first, so the commit of the
/// candidate's own block still reuses it. That other block's commit discards the candidate's
/// prepared commit, which was validated without it.
#[tokio::test]
async fn candidate_trees_survive_another_blocks_commit() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db);
    let sibling = nonce_variant(&block, [0xff; 32]);

    let (tip_sender, _latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    let prepared_commits = || {
        (
            PREPARED_COMMITS.load(Ordering::SeqCst),
            STALE_PREPARED_COMMITS.load(Ordering::SeqCst),
        )
    };
    let prepared_commits_before = prepared_commits();
    let (prepares_run_before, prepares_stored_before) = (
        PREPARES_RUN.load(Ordering::SeqCst),
        PREPARES_STORED.load(Ordering::SeqCst),
    );

    let (_guard, verification) = watch::channel(());
    let commit = block.clone().prepare();
    assert!(
        send_mining_candidate(tx, &commit, verification)
            .await
            .is_some(),
        "a block that extends the best tip gets work",
    );
    wait_for_prepares(tx, prepares_run_before + 1).await;
    assert_eq!(
        PREPARES_STORED.load(Ordering::SeqCst),
        prepares_stored_before + 1,
        "the block write task prepares the block's commit",
    );
    let committed = send_commit(tx, sibling.clone().prepare(), None).await;
    assert_eq!(committed.expect("the sibling is valid"), sibling.hash());

    let before = early_mining_work_counts();
    let committed = send_commit(tx, commit, None).await;
    assert_eq!(committed.expect("the block is valid"), block.hash());
    assert_eq!(
        prepared_commits(),
        prepared_commits_before,
        "the sibling's commit discards the candidate's prepared commit",
    );
    assert_eq!(
        early_mining_work_counts(),
        EarlyMiningWorkCounts {
            reused_trees: before.reused_trees + 1,
            reused_history_trees: before.reused_history_trees + 1,
            skipped_commitment_checks: before.skipped_commitment_checks + 1,
            ..before
        },
        "the commit reuses the candidate's work, although its sibling was committed first",
    );

    let published = state_receiver.borrow();
    assert!(published.any_chain_contains(&sibling.hash()));
    assert!(published.any_chain_contains(&block.hash()));
    drop(published);

    join_write_worker(sender, worker).await;
}

/// A mining candidate's work is kept when a solved block proposal is committed first, so the
/// commit of the candidate's own block still reuses it.
#[tokio::test]
async fn candidate_trees_survive_a_solved_proposal_commit() {
    let _init_guard = zebra_test::init();

    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block = block1_on_genesis(&state, &finalized.db);
    let proposal = nonce_variant(&block, [1; 32]);
    let solved = nonce_variant(&block, [2; 32]);

    let (tip_sender, _latest, _change) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let db = finalized.db.clone();
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    let (_guard, verification) = watch::channel(());
    assert!(
        send_mining_candidate(tx, &block.clone().prepare(), verification)
            .await
            .is_some(),
        "a block that extends the best tip gets work",
    );

    // Validate a proposal on a snapshot of the state, like the read state service does, and
    // commit a block that solves it with the result.
    let mut snapshot = state_receiver.borrow().clone();
    validate_and_commit_non_finalized(&db, &mut snapshot, proposal.clone().prepare())
        .expect("the proposal is contextually valid");
    let mut rebound = snapshot
        .remove_chain_with_tip(proposal.hash())
        .expect("validation adds the proposal to a chain")
        .tip_block()
        .expect("the proposal is the chain's tip")
        .clone();
    rebound.rebind(solved.clone(), std::time::Instant::now());
    let committed = send_commit(tx, solved.clone().prepare(), Some(Box::new(rebound))).await;
    assert_eq!(committed.expect("the solved block is valid"), solved.hash());

    let before = early_mining_work_counts();
    let committed = send_commit(tx, block.clone().prepare(), None).await;
    assert_eq!(committed.expect("the block is valid"), block.hash());
    assert_eq!(
        early_mining_work_counts(),
        EarlyMiningWorkCounts {
            reused_trees: before.reused_trees + 1,
            reused_history_trees: before.reused_history_trees + 1,
            skipped_commitment_checks: before.skipped_commitment_checks + 1,
            ..before
        },
        "the commit reuses the candidate's work, although a solved proposal was committed first",
    );

    let published = state_receiver.borrow();
    assert!(published.any_chain_contains(&solved.hash()));
    assert!(published.any_chain_contains(&block.hash()));
    drop(published);

    join_write_worker(sender, worker).await;
}

/// A mining candidate's work is kept when a sibling is committed first on a non-finalized parent,
/// so the commit of the candidate's own block, which forks the sibling's chain at that parent,
/// still reuses it.
#[tokio::test]
async fn candidate_trees_survive_a_sibling_commit_on_a_non_finalized_parent() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block1 = block1_on_genesis(&state, &finalized.db);
    let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("block is valid");

    let (tip_sender, _latest, _change) = ChainTipSender::new(None, &network);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let db = finalized.db.clone();
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    let committed = send_commit(tx, block1.clone().prepare(), None).await;
    assert_eq!(committed.expect("block 1 is valid"), block1.hash());

    // Give Mainnet block 2 a header that is contextually valid on block 1, as a block template
    // has.
    let snapshot = state_receiver.borrow().clone();
    let context = read::difficulty::get_block_template_chain_info(&snapshot, &db, &network)
        .expect("the state has a context for the next block");
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = context.tip_hash;
    header.time = context.min_time.into();
    header.difficulty_threshold = context.expected_difficulty;
    let block = Arc::new(block);
    let sibling = nonce_variant(&block, [0xff; 32]);

    let (_guard, verification) = watch::channel(());
    let commit = block.clone().prepare();
    assert!(
        send_mining_candidate(tx, &commit, verification)
            .await
            .is_some(),
        "a block that extends the best tip gets work",
    );
    // The sibling extends block 1's chain, so the block's commit forks that chain at block 1.
    let committed = send_commit(tx, sibling.clone().prepare(), None).await;
    assert_eq!(committed.expect("the sibling is valid"), sibling.hash());

    let before = early_mining_work_counts();
    let committed = send_commit(tx, commit, None).await;
    assert_eq!(committed.expect("the block is valid"), block.hash());
    assert_eq!(
        early_mining_work_counts(),
        EarlyMiningWorkCounts {
            reused_trees: before.reused_trees + 1,
            reused_history_trees: before.reused_history_trees + 1,
            skipped_commitment_checks: before.skipped_commitment_checks + 1,
            ..before
        },
        "the commit reuses the candidate's work on a fork of its sibling's chain",
    );

    let published = state_receiver.borrow();
    assert_eq!(
        published.chain_count(),
        2,
        "the block forks its sibling's chain"
    );
    assert!(published.any_chain_contains(&sibling.hash()));
    assert!(published.any_chain_contains(&block.hash()));
    drop(published);

    join_write_worker(sender, worker).await;
}

/// A block that was validated as a proposal is committed by rebinding the chain its validation
/// built, and any other block is committed as usual.
#[tokio::test]
async fn write_worker_rebinds_a_solved_proposal() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let [block1, block2]: [Block; 2] = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
    ]
    .map(|bytes| bytes.zcash_deserialize_into().expect("block is valid"));

    let (tip_sender, latest, _change) = ChainTipSender::new(None, &network);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let db = finalized.db.clone();
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    // Gives `block` a header that is contextually valid on the best tip, as a block template has.
    let on_best_tip = |mut block: Block| {
        let snapshot = state_receiver.borrow().clone();
        let context = read::difficulty::get_block_template_chain_info(&snapshot, &db, &network)
            .expect("the state has a context for the next block");
        let header = Arc::make_mut(&mut block.header);
        header.previous_block_hash = context.tip_hash;
        header.time = context.min_time.into();
        header.difficulty_threshold = context.expected_difficulty;
        Arc::new(block)
    };
    let commit = |block: SemanticallyVerifiedBlock,
                  contextual: Option<Box<ContextuallyVerifiedBlock>>| async {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        tx.send(NonFinalizedWriteMessage::Commit((
            block, rsp_tx, contextual,
        )))
        .unwrap();

        tokio::time::timeout(Duration::from_secs(10), rsp_rx)
            .await
            .expect("the write task answers")
            .expect("the write task is running")
    };

    // The proposal's parent has to be in the non-finalized state for its chain to be rebound.
    let block1 = on_best_tip(block1);
    assert_eq!(
        commit(block1.clone().prepare(), None).await.ok(),
        Some(block1.hash())
    );

    // Validate a proposal on a snapshot of the state, like the read state service does.
    let proposal = on_best_tip(block2);
    let mut snapshot = state_receiver.borrow().clone();
    let mut expected = snapshot.clone();
    validate_and_commit_non_finalized(&db, &mut snapshot, proposal.clone().prepare())
        .expect("the proposal is contextually valid");
    let prepared_chain = snapshot
        .remove_chain_with_tip(proposal.hash())
        .expect("validation adds the proposal to a chain");
    let contextual = prepared_chain
        .tip_block()
        .expect("the proposal is the chain's tip")
        .clone();
    snapshot.store_prepared_chain(prepared_chain);
    assert!(snapshot.has_prepared_chain());

    // A solved proposal only differs in its nonce and solution.
    let mut solved = (*proposal).clone();
    Arc::make_mut(&mut solved.header).nonce = [0xff; 32].into();
    let solved = Arc::new(solved);
    assert_ne!(solved.hash(), proposal.hash());

    let received_time = std::time::Instant::now();
    let mut rebound = contextual.clone();
    rebound.rebind(solved.clone(), received_time);
    let semantic = solved.clone().prepare();

    let committed = commit(semantic, Some(Box::new(rebound))).await;
    assert_eq!(committed.ok(), Some(solved.hash()));
    assert_eq!(latest.best_tip_hash(), Some(solved.hash()));
    assert!(
        !snapshot.has_prepared_chain(),
        "the commit rebinds the prepared chain instead of pushing the block",
    );

    // The rebound chain is the same as a chain that the solved block was pushed onto.
    let mut pushed = solved.clone().prepare();
    pushed.received_time = Some(received_time);
    validate_and_commit_non_finalized(&db, &mut expected, pushed)
        .expect("the solved block is contextually valid");
    let best_chain = state_receiver
        .borrow()
        .best_chain()
        .expect("the state has a best chain")
        .clone();
    assert!(
        best_chain.eq_internal_state(expected.best_chain().expect("the state has a best chain")),
        "rebinding gives the same chain as pushing the solved block",
    );

    join_write_worker(sender, worker).await;
}

/// The block write task drops a prepared chain once the best chain is longer than it, and keeps
/// it while a block that solves its proposal could still extend the best chain.
#[tokio::test]
async fn write_worker_drops_a_prepared_chain_that_the_best_chain_passed() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let [block1, block2, block3]: [Block; 3] = [
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_3_BYTES[..],
    ]
    .map(|bytes| bytes.zcash_deserialize_into().expect("block is valid"));

    let (tip_sender, latest, _change) = ChainTipSender::new(None, &network);
    let (state_sender, state_receiver) = watch::channel(state.clone());
    let db = finalized.db.clone();
    let (sender, _resets, _rejections, worker) =
        BlockWriteSender::spawn(finalized, state, tip_sender, state_sender, false, None);
    let tx = sender.non_finalized.as_ref().unwrap();

    // Gives `block` a header that is contextually valid on the best tip, as a block template has.
    let on_best_tip = |mut block: Block| {
        let snapshot = state_receiver.borrow().clone();
        let context = read::difficulty::get_block_template_chain_info(&snapshot, &db, &network)
            .expect("the state has a context for the next block");
        let header = Arc::make_mut(&mut block.header);
        header.previous_block_hash = context.tip_hash;
        header.time = context.min_time.into();
        header.difficulty_threshold = context.expected_difficulty;
        Arc::new(block)
    };
    let commit = |block: Arc<Block>| async {
        let hash = block.hash();
        let (rsp_tx, rsp_rx) = oneshot::channel();
        tx.send(NonFinalizedWriteMessage::Commit((
            block.prepare(),
            rsp_tx,
            None,
        )))
        .unwrap();

        let committed = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
            .await
            .expect("the write task answers")
            .expect("the write task is running");
        assert_eq!(committed.ok(), Some(hash));
    };
    // The write task drops a passed prepared chain after it answers the commit. It handles one
    // request at a time, so it has finished with that commit once it answers another request.
    let after_last_commit = || async {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        tx.send(NonFinalizedWriteMessage::Invalidate {
            hash: block::Hash([0xff; 32]),
            rsp_tx,
        })
        .unwrap();

        let invalidated = tokio::time::timeout(Duration::from_secs(10), rsp_rx)
            .await
            .expect("the write task answers")
            .expect("the write task is running");
        assert!(invalidated.is_err(), "no block has this hash");
    };

    commit(on_best_tip(block1)).await;

    // Validate a proposal on a snapshot of the state, like the read state service does.
    let proposal = on_best_tip(block2);
    let mut snapshot = state_receiver.borrow().clone();
    validate_and_commit_non_finalized(&db, &mut snapshot, proposal.clone().prepare())
        .expect("the proposal is contextually valid");
    let prepared_chain = snapshot
        .remove_chain_with_tip(proposal.hash())
        .expect("validation adds the proposal to a chain");
    snapshot.store_prepared_chain(prepared_chain);

    // Another block at the proposal's height leaves the prepared chain as long as the best chain.
    let mut sibling = (*proposal).clone();
    Arc::make_mut(&mut sibling.header).nonce = [0xff; 32].into();
    let sibling = Arc::new(sibling);
    commit(sibling.clone()).await;
    after_last_commit().await;
    assert_eq!(latest.best_tip_hash(), Some(sibling.hash()));
    assert!(snapshot.has_prepared_chain());

    // A block above the proposal's height makes the best chain longer.
    commit(on_best_tip(block3)).await;
    after_last_commit().await;
    assert!(
        !snapshot.has_prepared_chain(),
        "a proposal below the best tip can't extend the best chain",
    );

    join_write_worker(sender, worker).await;
}

mod prepared;
