//! Tests for the block write task.

use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::sync::{oneshot, watch};

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};

use crate::{
    arbitrary::Prepare,
    constants::MAX_NON_FINALIZED_CHAIN_FORKS,
    service::{chain_tip::ChainTipSender, non_finalized_state::NonFinalizedState},
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
};

use super::BlockWriteSender;

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// When a block's parent was evicted from the non-finalized state, the write task must
/// report the parent hash as well as the block hash, so the state service stops treating
/// the parent as sent and it can be downloaded again.
#[test]
fn evicted_parent_is_reported_when_its_child_arrives() {
    let _init_guard = zebra_test::init();

    let (finalized_state, non_finalized_state, _genesis) = new_state_with_mainnet_genesis();
    let network = non_finalized_state.network.clone();
    let (chain_tip_sender, _latest_chain_tip, _chain_tip_change) =
        ChainTipSender::new(None, &network);
    let (non_finalized_state_sender, non_finalized_state_receiver) =
        watch::channel(NonFinalizedState::new(&network));

    let (block_write_sender, _invalid_block_reset_receiver, mut rejected_receiver, _task) =
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

    let commit = |block: Arc<Block>, received_time| {
        let mut prepared = block.prepare();
        prepared.received_time = received_time;

        let (rsp_tx, rsp_rx) = oneshot::channel();
        non_finalized_block_write_sender
            .send((prepared, rsp_tx).into())
            .expect("write task is running");
        rsp_rx.blocking_recv().expect("write task responds")
    };

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("block should deserialize");

    // Equal-work siblings of block 1, one more than the chain set can hold.
    let received = std::time::Instant::now();
    let max_forks =
        u8::try_from(MAX_NON_FINALIZED_CHAIN_FORKS).expect("the fork limit fits in a u8");
    let siblings: Vec<_> = (0..=max_forks)
        .map(|i| block1.clone().set_block_commitment([i; 32]))
        .collect();

    for (i, sibling) in (0..=max_forks).zip(&siblings) {
        commit(
            sibling.clone(),
            Some(received + Duration::from_secs(i.into())),
        )
        .expect("sibling should commit");
    }

    let latest_non_finalized_state = non_finalized_state_receiver.borrow().clone();
    let evicted: Vec<_> = siblings
        .iter()
        .filter(|sibling| !latest_non_finalized_state.any_chain_contains(&sibling.hash()))
        .collect();
    assert_eq!(1, evicted.len(), "exactly one sibling is evicted");
    let evicted = evicted[0];

    // The network builds on the evicted sibling.
    let child = evicted.make_fake_child();
    commit(child.clone(), None).expect_err("the child's parent is not in any chain");

    let reported: HashSet<_> = std::iter::from_fn(|| rejected_receiver.try_recv().ok()).collect();
    assert_eq!(
        HashSet::from([child.hash(), evicted.hash()]),
        reported,
        "the evicted parent must be reported along with its child"
    );
}
