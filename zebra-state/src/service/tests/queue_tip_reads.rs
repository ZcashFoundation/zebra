//! The state reads the finalized tip from the database at most once to queue a block for the
//! block write task.

use std::{cell::Cell, sync::Arc};

use zebra_chain::block::{self, Block};

use crate::{
    service::{finalized_state::FINALIZED_TIP_READS, StateService},
    CheckpointVerifiedBlock,
};

use super::mining_rejection::{candidate, state};

/// Queues `block` on `state`, and returns the number of finalized tip reads that made on this
/// thread.
fn tip_reads_to_queue(state: &mut StateService, block: &Arc<Block>) -> usize {
    let before = FINALIZED_TIP_READS.with(Cell::get);
    let _response = state.queue_and_commit_to_non_finalized_state(
        CheckpointVerifiedBlock::from(block.clone()).into(),
        None,
        None,
    );

    FINALIZED_TIP_READS.with(Cell::get) - before
}

/// Returns `true` if `state` sent `block` to the block write task, and `false` if the block is
/// waiting for its parent.
fn is_sent(state: &StateService, block: &Arc<Block>) -> bool {
    let sent = state
        .non_finalized_block_write_sent_hashes
        .contains(&block.hash());
    assert_ne!(
        sent,
        state.non_finalized_state_queued_blocks.has(block.hash()),
        "a block is either sent or waiting",
    );

    sent
}

/// Queueing a semantically verified block reads the finalized tip once, whether its parent was
/// sent to the block write task, is the finalized tip, or is unknown. The tip is a database
/// read, and the block is queued on the state's own task.
///
/// Only a block with a sent parent, or with the finalized tip as its parent, is sent on.
#[test]
fn queueing_a_block_reads_the_finalized_tip_once() {
    let _init_guard = zebra_test::init();

    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    // The state has a finalized genesis block and one non-finalized block, which it sent to the
    // block write task.
    let (mut queueing, child_of_sent, child_of_finalized_tip) = setup.block_on(async {
        let (queueing, read) = state(false).await;
        let child_of_sent = candidate(&read, false, 0).await;
        // Another block at the non-finalized block's height: a later nonce gives it its own
        // hash.
        let (_finalized_only, finalized_only_read) = state(true).await;
        let child_of_finalized_tip = candidate(&finalized_only_read, false, 1).await;
        (queueing, child_of_sent, child_of_finalized_tip)
    });
    let mut orphan = Block::clone(&child_of_sent);
    Arc::make_mut(&mut orphan.header).previous_block_hash = block::Hash([7; 32]);
    let orphan = Arc::new(orphan);

    // An unknown parent is compared with the finalized tip, and the block waits for it.
    assert_eq!(tip_reads_to_queue(&mut queueing, &orphan), 1);
    assert!(!is_sent(&queueing, &orphan));
    // The finalized tip that the parent is compared with also gives the height to prune at.
    assert_eq!(
        tip_reads_to_queue(&mut queueing, &child_of_finalized_tip),
        1
    );
    assert!(is_sent(&queueing, &child_of_finalized_tip));
    // A parent that was sent is found in memory, so the tip is only read to prune.
    assert_eq!(tip_reads_to_queue(&mut queueing, &child_of_sent), 1);
    assert!(is_sent(&queueing, &child_of_sent));

    drop(setup);
}
