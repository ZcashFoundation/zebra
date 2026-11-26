//! Tests for the `ChainTips` read request.

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use zebra_chain::{
    amount::NonNegative, block::Block, parameters::Network::Mainnet,
    serialization::ZcashDeserializeInto, value_balance::ValueBalance,
};
use zebra_test::{prelude::Result, transcript::Transcript};

use crate::{
    arbitrary::Prepare,
    init_test_services, populated_state,
    service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
    tests::FakeChainHelper,
    ChainTipInfo,
    ChainTipStatus::{self, Active, ValidFork},
    Config, ReadRequest, ReadResponse,
};

use super::chain_tips;

/// A state without blocks has no chain tips.
#[tokio::test]
async fn empty_state_has_no_chain_tips() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        init_test_services(&Mainnet).await;

    Transcript::from([(
        ReadRequest::ChainTips,
        Ok(ReadResponse::ChainTips(Vec::new())),
    )])
    .check(read_state)
    .await
}

/// Without non-finalized blocks, the finalized tip is the only chain tip.
#[tokio::test(flavor = "multi_thread")]
async fn finalized_tip_is_the_only_tip_without_non_finalized_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();
    let finalized_tip = tip(
        blocks.last().expect("the test vectors have blocks"),
        0,
        Active,
    );

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks, &Mainnet).await;

    Transcript::from([(
        ReadRequest::ChainTips,
        Ok(ReadResponse::ChainTips(vec![finalized_tip])),
    )])
    .check(read_state)
    .await
}

/// The best chain tip is `Active` with no branch, and each side chain tip is a `ValidFork` whose
/// branch is the blocks after its fork point with the best chain, even when it forks at the
/// finalized tip. Side chains that fork below the finalized tip are gone.
#[test]
fn best_and_side_chain_tips() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut state = NonFinalizedState::new(&Mainnet);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &Mainnet)
        .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    // Pre-Heartwood blocks don't need the history tree that the empty finalized state lacks.
    let root: Arc<Block> = Arc::new(Mainnet.test_block(653599, 583999).unwrap());
    let best_1 = root.make_fake_child().set_work(100);
    let best_2 = best_1.make_fake_child().set_work(100);
    let best_3 = best_2.make_fake_child().set_work(100);

    state.commit_new_chain(root.clone().prepare(), &finalized_state)?;
    for block in [&best_1, &best_2, &best_3] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    assert_eq!(
        chain_tips(&state, &finalized_state.db),
        [tip(&best_3, 0, Active)]
    );

    // A side chain that forks after `best_1`.
    let side = best_1.make_fake_child().set_work(50);
    state.commit_block(side.clone().prepare(), &finalized_state)?;
    assert_eq!(
        chain_tips(&state, &finalized_state.db),
        [tip(&best_3, 0, Active), tip(&side, 1, ValidFork)]
    );

    // A side chain that forks after `root`, the first non-finalized block.
    let fork_1 = root.make_fake_child().set_work(20);
    let fork_2 = fork_1.make_fake_child().set_work(20);
    state.commit_block(fork_1.prepare(), &finalized_state)?;
    state.commit_block(fork_2.clone().prepare(), &finalized_state)?;

    // A side chain that forks at the finalized tip, so it has no blocks in the best chain.
    let other_root = root.clone().set_work(5);
    let other_child = other_root.make_fake_child().set_work(5);
    state.commit_new_chain(other_root.prepare(), &finalized_state)?;
    state.commit_block(other_child.clone().prepare(), &finalized_state)?;

    assert_eq!(
        chain_tips(&state, &finalized_state.db),
        [
            tip(&best_3, 0, Active),
            tip(&side, 1, ValidFork),
            tip(&fork_2, 2, ValidFork),
            tip(&other_child, 2, ValidFork),
        ]
    );

    // Finalizing `root` drops the side chain that forked below it, as the reorg limit does.
    state.finalize();
    assert_eq!(
        chain_tips(&state, &finalized_state.db),
        [
            tip(&best_3, 0, Active),
            tip(&side, 1, ValidFork),
            tip(&fork_2, 2, ValidFork),
        ]
    );

    Ok(())
}

/// A chain that ends at a block another chain extends has no tip of its own. Invalidating a
/// block leaves one behind when a side chain forks below the invalidated block.
#[test]
fn chains_extended_by_another_chain_have_no_tip() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut state = NonFinalizedState::new(&Mainnet);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &Mainnet)
        .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    let root: Arc<Block> = Arc::new(Mainnet.test_block(653599, 583999).unwrap());
    let parent = root.make_fake_child().set_work(10);
    let invalid = parent.make_fake_child().set_work(10);
    let invalid_child = invalid.make_fake_child().set_work(10);
    let side = parent.make_fake_child().set_work(5);

    state.commit_new_chain(root.prepare(), &finalized_state)?;
    for block in [&parent, &invalid, &invalid_child, &side] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    state.invalidate_block(invalid.hash())?;

    // One chain now ends at `parent`, and `side` extends it.
    assert_eq!(state.chain_count(), 2);
    assert_eq!(
        chain_tips(&state, &finalized_state.db),
        [tip(&side, 0, Active)]
    );

    Ok(())
}

/// Returns the chain tip at `block`.
fn tip(block: &Block, branch_len: usize, status: ChainTipStatus) -> ChainTipInfo {
    ChainTipInfo {
        height: block
            .coinbase_height()
            .expect("test blocks have a coinbase height"),
        hash: block.hash(),
        branch_len,
        status,
    }
}
