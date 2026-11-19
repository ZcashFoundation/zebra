//! A block prepared before its commit request must commit the chain an unprepared block does, and
//! only for the block object that was prepared, on the state it was prepared on.

use std::cell::Cell;

use zebra_chain::serialization::ZcashDeserializeInto;

use super::*;

/// Returns a state whose only non-finalized block is Mainnet block 1, and that block.
fn root_state() -> (FinalizedState, NonFinalizedState, Arc<Block>) {
    let (finalized, mut state, _genesis) = new_state_with_mainnet_genesis();
    let root: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state
        .commit_new_chain(root.clone().prepare(), &finalized.db)
        .unwrap();

    (finalized, state, root)
}

/// Runs a block write task over `messages` on this thread, and returns the state it published.
fn run_writer(
    finalized: FinalizedState,
    state: NonFinalizedState,
    messages: Vec<NonFinalizedWriteMessage>,
) -> NonFinalizedState {
    let (chain_tip_sender, _tip, _) = ChainTipSender::new(None, &Network::Mainnet);
    let (non_finalized_state_sender, snapshot) = watch::channel(state.clone());
    let (queued, non_finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    for message in messages {
        assert!(queued.send(message).is_ok());
    }
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
        tree_roots_cache: Some(no_fast_tree_sync()),
        pruning_wake: None,
    };
    worker.run();

    let published = snapshot.borrow().clone();
    published
}

/// Returns the number of prepared blocks `run` committed on this thread.
fn prepared_commits<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = PREPARED_COMMIT_COUNT.with(Cell::get);
    let output = run();

    (output, PREPARED_COMMIT_COUNT.with(Cell::get) - before)
}

#[test]
fn prepared_block_commits_the_chain_an_unprepared_block_does() {
    let _init_guard = zebra_test::init();
    let received_time = Instant::now();

    let mut published = Vec::new();
    for prepare in [true, false] {
        let (finalized, state, root) = root_state();
        let child = root.make_fake_child();
        let mut semantic = child.clone().prepare();
        semantic.received_time = Some(received_time);

        let (rsp_tx, mut rsp_rx) = oneshot::channel();
        let mut messages = Vec::new();
        if prepare {
            messages.push(NonFinalizedWriteMessage::Prepare(child.clone()));
        }
        messages.push(NonFinalizedWriteMessage::Commit(
            (semantic, rsp_tx, None, None),
            Instant::now(),
        ));

        let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

        assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), child.hash());
        assert_eq!(prepared_commits, usize::from(prepare));
        let tip = state.best_chain().unwrap().tip_block().unwrap();
        assert!(Arc::ptr_eq(&tip.block, &child));
        assert_eq!(
            tip.received_time,
            Some(received_time),
            "a prepared block is committed with its commit request's received time",
        );
        published.push(state);
    }

    assert!(published[0].eq_internal_state(&published[1]));
}

#[test]
fn request_between_prepare_and_commit_discards_the_prepared_block() {
    let _init_guard = zebra_test::init();
    let (finalized, mut state, root) = root_state();
    let child = root.make_fake_child();
    state
        .commit_block(child.clone().prepare(), &finalized.db)
        .unwrap();
    let grandchild = child.make_fake_child();

    let (invalidated_tx, mut invalidated_rx) = oneshot::channel();
    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(grandchild.clone()),
        // The prepared chain has the grandchild on top of this block.
        NonFinalizedWriteMessage::Invalidate {
            hash: child.hash(),
            rsp_tx: invalidated_tx,
        },
        NonFinalizedWriteMessage::Commit(
            (grandchild.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];

    let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

    assert_eq!(invalidated_rx.try_recv().unwrap().unwrap(), child.hash());
    assert!(
        rsp_rx.try_recv().unwrap().is_err(),
        "a block can't be committed on top of an invalidated parent",
    );
    assert_eq!(prepared_commits, 0);
    assert_eq!(state.best_tip().unwrap().1, root.hash());
    assert!(!state.any_chain_contains(&grandchild.hash()));
}

#[test]
fn only_the_prepared_block_object_commits_the_prepared_chain() {
    let _init_guard = zebra_test::init();
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child();
    // A block hash doesn't identify a block's authorizing data, so an equal block isn't enough.
    let twin = Arc::new(Block::clone(&child));
    assert_eq!(twin.hash(), child.hash());

    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(child.clone()),
        NonFinalizedWriteMessage::Commit(
            (twin.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];

    let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), twin.hash());
    assert_eq!(prepared_commits, 0);
    let tip = state.best_chain().unwrap().tip_block().unwrap();
    assert!(Arc::ptr_eq(&tip.block, &twin));
    assert!(!Arc::ptr_eq(&tip.block, &child));
}

#[test]
fn block_that_cannot_be_prepared_is_validated_by_its_commit() {
    let _init_guard = zebra_test::init();
    let (finalized, state, root) = root_state();
    // The state has no parent for this block, so it can't be prepared or committed.
    let orphan = root.make_fake_child().make_fake_child();
    // A block without a coinbase height can't be validated at all.
    let mut without_coinbase = Block::clone(&root);
    without_coinbase.transactions.clear();
    // A chain can't index a block that repeats a transaction.
    let mut repeated_transaction = Block::clone(&root.make_fake_child());
    let repeated = repeated_transaction.transactions[0].clone();
    repeated_transaction.transactions.push(repeated);

    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(Arc::new(without_coinbase)),
        NonFinalizedWriteMessage::Prepare(Arc::new(repeated_transaction)),
        NonFinalizedWriteMessage::Prepare(orphan.clone()),
        NonFinalizedWriteMessage::Commit(
            (orphan.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];

    let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

    assert!(rsp_rx.try_recv().unwrap().is_err());
    assert_eq!(prepared_commits, 0);
    assert_eq!(state.best_tip().unwrap().1, root.hash());
}

#[test]
fn commit_of_another_block_discards_the_prepared_block() {
    let _init_guard = zebra_test::init();
    let (finalized, state, root) = root_state();
    let child = root.make_fake_child().set_work(1);
    let sibling = root.make_fake_child().set_work(2);
    assert_ne!(child.hash(), sibling.hash());

    let (sibling_tx, mut sibling_rx) = oneshot::channel();
    let (child_tx, mut child_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(child.clone()),
        NonFinalizedWriteMessage::Commit(
            (sibling.clone().prepare(), sibling_tx, None, None),
            Instant::now(),
        ),
        NonFinalizedWriteMessage::Commit(
            (child.clone().prepare(), child_tx, None, None),
            Instant::now(),
        ),
    ];

    let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

    assert_eq!(sibling_rx.try_recv().unwrap().unwrap(), sibling.hash());
    assert_eq!(child_rx.try_recv().unwrap().unwrap(), child.hash());
    assert_eq!(
        prepared_commits, 0,
        "the child was prepared on a state without its sibling",
    );
    assert!(state.any_chain_contains(&child.hash()));
    assert!(state.any_chain_contains(&sibling.hash()));
}

#[test]
fn prepared_block_on_the_finalized_tip_starts_a_chain() {
    let _init_guard = zebra_test::init();
    let (finalized, state, _genesis) = new_state_with_mainnet_genesis();
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();

    let (rsp_tx, mut rsp_rx) = oneshot::channel();
    let messages = vec![
        NonFinalizedWriteMessage::Prepare(block1.clone()),
        NonFinalizedWriteMessage::Commit(
            (block1.clone().prepare(), rsp_tx, None, None),
            Instant::now(),
        ),
    ];

    let (state, prepared_commits) = prepared_commits(|| run_writer(finalized, state, messages));

    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), block1.hash());
    assert_eq!(prepared_commits, 1);
    assert_eq!(state.chain_count(), 1);
    assert_eq!(state.best_tip().unwrap().1, block1.hash());
}
