//! The state queues a block for the block write task on its own task.

use std::sync::Arc;

use tower::{Service, ServiceExt};
use zebra_chain::block::{self, Block};

use crate::{CheckpointVerifiedBlock, Request, Response};

use super::mining_rejection::{candidate, state};

/// A semantically verified block that is queued alone must be queued without handing the worker
/// thread over (`tokio::task::block_in_place`). The state only reads a few heights before it
/// sends the block to the block write task, and a task that has handed its thread over can only
/// wake its caller by way of another thread, which takes longer than the rest of the commit.
///
/// Handing the thread over panics on a runtime with a single thread, so the block is committed
/// on one.
#[test]
fn semantically_verified_block_is_queued_on_the_state_task() {
    let _init_guard = zebra_test::init();

    // The state is created on its own runtime: committing its first blocks needs blocking
    // threads, and a multi-threaded runtime.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mut state, block) = setup.block_on(async {
        let (state, read) = state(false).await;
        let block = candidate(&read, false, 0).await;
        (state, block)
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let response = state
                .ready()
                .await
                .unwrap()
                .call(Request::CommitSemanticallyVerifiedBlock(
                    CheckpointVerifiedBlock::from(block.clone()).into(),
                ))
                .await
                .expect("a child of the best tip is committed");
            assert!(matches!(response, Response::Committed(hash) if hash == block.hash()));
        });
    drop(setup);
}

/// The thread is only kept for a block that is queued alone. A block can release the blocks
/// that are queued behind it, and the first block after the checkpoints ends their queue: both
/// do more work, so the thread is handed over for them.
#[test]
fn only_a_block_queued_alone_keeps_the_thread() {
    let _init_guard = zebra_test::init();

    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mut state, block, finalized_only) = setup.block_on(async {
        let (state, read) = state(false).await;
        let block = candidate(&read, false, 0).await;
        let (finalized_only, _read) = super::mining_rejection::state(true).await;
        (state, block, finalized_only)
    });

    assert!(state.queues_block_alone());
    // The checkpoint verified blocks' queue ends with the first semantically verified block.
    assert!(!finalized_only.queues_block_alone());

    let (response, _receiver) = tokio::sync::oneshot::channel();
    state.non_finalized_state_queued_blocks.queue((
        CheckpointVerifiedBlock::from(block).into(),
        response,
        None,
        None,
    ));
    assert!(!state.queues_block_alone());
    drop(setup);
}

/// A block that isn't queued alone must have the worker thread handed over while it is queued
/// (`tokio::task::block_in_place`), so the thread's other tasks keep running.
///
/// Handing the thread over panics on a runtime with a single thread, so committing such a block
/// on one panics.
#[test]
#[should_panic(expected = "can call blocking only when running on the multi-threaded runtime")]
fn thread_is_handed_over_for_a_block_that_is_not_queued_alone() {
    let _init_guard = zebra_test::init();

    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mut state, block) = setup.block_on(async {
        let (state, read) = state(false).await;
        let block = candidate(&read, false, 0).await;
        (state, block)
    });

    // Another block is waiting for its parent.
    let mut waiting = Block::clone(&block);
    Arc::make_mut(&mut waiting.header).previous_block_hash = block::Hash([7; 32]);
    let (response, _receiver) = tokio::sync::oneshot::channel();
    state.non_finalized_state_queued_blocks.queue((
        CheckpointVerifiedBlock::from(Arc::new(waiting)).into(),
        response,
        None,
        None,
    ));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _response =
                state
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::CommitSemanticallyVerifiedBlock(
                        CheckpointVerifiedBlock::from(block).into(),
                    ));
        });
}
