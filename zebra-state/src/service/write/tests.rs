//! Tests for the block write task.

use std::{collections::HashSet, sync::Arc};

use tokio::sync::{oneshot, watch};

use zebra_chain::{
    block::{self, Block},
    serialization::ZcashDeserializeInto,
};

use crate::{
    arbitrary::Prepare,
    constants::MAX_NON_FINALIZED_CHAIN_FORKS,
    service::{chain_tip::ChainTipSender, non_finalized_state::NonFinalizedState},
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
};

use super::{BlockWriteSender, NonFinalizedWriteMessage};

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// The write task keeps blocks that the fork limit evicts marked as sent until a child of
/// them arrives, then reports their whole evicted branch, but never a shared ancestor or the
/// parent of a block that fails because its parent was invalidated.
#[test]
fn evicted_branch_is_reported_when_its_child_arrives() {
    let _init_guard = zebra_test::init();

    let (finalized_state, non_finalized_state, _genesis) = new_state_with_mainnet_genesis();
    let network = non_finalized_state.network.clone();
    let (chain_tip_sender, _latest_chain_tip, _chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (non_finalized_state_sender, non_finalized_state_receiver) =
        watch::channel(NonFinalizedState::new(&network));

    let (block_write_sender, _invalid_block_reset_receiver, mut forget_receiver, _task) =
        BlockWriteSender::spawn(
            finalized_state,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
            false,
            None,
        );
    let non_finalized_block_write_sender = block_write_sender
        .non_finalized
        .expect("non-finalized block write channel is open");

    let commit = |block: &Arc<Block>| {
        let (rsp_tx, rsp_rx) = oneshot::channel();
        non_finalized_block_write_sender
            .send((block.clone().prepare(), rsp_tx).into())
            .expect("write task is running");
        rsp_rx.blocking_recv().expect("write task responds")
    };
    let mut reported =
        || -> HashSet<_> { std::iter::from_fn(|| forget_receiver.try_recv().ok()).collect() };

    let [block1, block2, block3, block4]: [Arc<Block>; 4] = [
        &*zebra_test::vectors::BLOCK_MAINNET_1_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_2_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_3_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_4_BYTES,
    ]
    .map(|bytes| {
        bytes
            .zcash_deserialize_into()
            .expect("block should deserialize")
    });
    let with_parent = |block: &Arc<Block>, parent_hash: block::Hash| {
        let mut block = Block::clone(block);
        Arc::make_mut(&mut block.header).previous_block_hash = parent_hash;
        Arc::new(block)
    };

    // A two-block branch on block 1, then longer chains until the fork limit evicts it.
    let low2 = block2.clone().set_block_commitment([0xff; 32]);
    let low3 = with_parent(&block3, low2.hash());
    for block in [&block1, &low2, &low3, &block2, &block3] {
        commit(block).expect("block should commit");
    }
    let max_forks =
        u8::try_from(MAX_NON_FINALIZED_CHAIN_FORKS).expect("the fork limit fits in a u8");
    for i in 0..max_forks {
        commit(&block4.clone().set_block_commitment([i; 32])).expect("block should commit");
    }

    assert!(!non_finalized_state_receiver
        .borrow()
        .any_chain_contains(&low2.hash()));
    assert!(reported().is_empty(), "evicted blocks stay marked as sent");

    let low_child = low3.make_fake_child();
    commit(&low_child).expect_err("the child's parent is not in any chain");
    assert_eq!(
        HashSet::from([low_child.hash(), low3.hash(), low2.hash()]),
        reported(),
        "the whole evicted branch is reported, but not the shared block 1"
    );

    // A child of an invalidated block is reported, but the invalidated block is not.
    let invalidated = block4.clone().set_block_commitment([0; 32]);
    let (rsp_tx, rsp_rx) = oneshot::channel();
    non_finalized_block_write_sender
        .send(NonFinalizedWriteMessage::Invalidate {
            hash: invalidated.hash(),
            rsp_tx,
        })
        .expect("write task is running");
    rsp_rx
        .blocking_recv()
        .expect("write task responds")
        .expect("block should be invalidated");

    let child = invalidated.make_fake_child();
    commit(&child).expect_err("the child's parent is not in any chain");
    assert_eq!(HashSet::from([child.hash()]), reported());
}
