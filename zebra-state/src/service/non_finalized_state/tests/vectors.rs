//! Fixed test vectors for the non-finalized state.

use std::{sync::Arc, time::Duration};

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{self, Block, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
    transaction::Transaction,
    transparent,
    value_balance::ValueBalance,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    request::ContextuallyVerifiedBlock,
    service::{
        finalized_state::{calculate_deferred_pool_balance_change, FinalizedState},
        non_finalized_state::{Chain, NonFinalizedState, MIN_DURATION_BETWEEN_BACKUP_UPDATES},
        ReconsiderError,
    },
    tests::FakeChainHelper,
    Config, SemanticallyVerifiedBlock,
};

#[test]
fn construct_empty() {
    let _init_guard = zebra_test::init();
    let _chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::zero(),
    );
}

#[test]
fn construct_single() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

    let mut chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;

    assert_eq!(1, chain.blocks.len());

    Ok(())
}

#[test]
fn construct_many() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let initial_height = block
        .coinbase_height()
        .expect("Block 434873 should have its height in its coinbase tx.");
    let mut blocks = vec![];

    while blocks.len() < 100 {
        let next_block = block.make_fake_child();
        blocks.push(block);
        block = next_block;
    }

    let mut chain = Chain::new(
        &Network::Mainnet,
        (initial_height - 1).expect("Initial height should be at least 1."),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    for block in blocks {
        chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;
    }

    assert_eq!(100, chain.blocks.len());

    Ok(())
}

#[test]
fn ord_matches_work() -> Result<()> {
    let _init_guard = zebra_test::init();
    let less_block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .set_work(1);
    let more_block = less_block.clone().set_work(10);

    let mut lesser_chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    lesser_chain = lesser_chain.push(less_block.prepare().test_with_zero_spent_utxos())?;

    let mut bigger_chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::zero(),
    );
    bigger_chain = bigger_chain.push(more_block.prepare().test_with_zero_spent_utxos())?;

    assert!(bigger_chain > lesser_chain);

    Ok(())
}

#[test]
fn best_chain_wins() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        best_chain_wins_for_network(network)?;
    }

    Ok(())
}

fn best_chain_wins_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let expected_hash = block2.hash();

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    state.commit_new_chain(block2.prepare(), &finalized_state)?;
    state.commit_new_chain(child.prepare(), &finalized_state)?;

    let best_chain = state.best_chain().unwrap();
    assert!(best_chain.height_by_hash.contains_key(&expected_hash));

    Ok(())
}

#[test]
fn finalize_pops_from_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        finalize_pops_from_best_chain_for_network(network)?;
    }

    Ok(())
}

fn finalize_pops_from_best_chain_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(child.prepare(), &finalized_state)?;

    let finalized = state.finalize().inner_block();

    assert_eq!(block1, finalized);

    let finalized = state.finalize().inner_block();
    assert_eq!(block2, finalized);

    assert!(state.best_chain().is_none());

    Ok(())
}

#[test]
fn invalidate_block_removes_block_and_descendants_from_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        invalidate_block_removes_block_and_descendants_from_chain_for_network(network)?;
    }

    Ok(())
}

fn invalidate_block_removes_block_and_descendants_from_chain_for_network(
    network: Network,
) -> Result<()> {
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(block3.clone().prepare(), &finalized_state)?;

    assert_eq!(
        state
            .best_chain()
            .unwrap_or(&Arc::new(Chain::default()))
            .blocks
            .len(),
        3
    );

    let _ = state.invalidate_block(block2.hash());

    let post_invalidated_chain = state.best_chain().unwrap();

    assert_eq!(post_invalidated_chain.blocks.len(), 1);
    assert!(
        post_invalidated_chain.contains_block_hash(block1.hash()),
        "the new modified chain should contain block1"
    );

    assert!(
        !post_invalidated_chain.contains_block_hash(block2.hash()),
        "the new modified chain should not contain block2"
    );
    assert!(
        !post_invalidated_chain.contains_block_hash(block3.hash()),
        "the new modified chain should not contain block3"
    );

    let invalidated_blocks_state = &state.invalidated_blocks;

    // Find an entry in the IndexMap that contains block2 hash
    let (_, invalidated_blocks_state_descendants) = invalidated_blocks_state
        .iter()
        .find_map(|(height, blocks)| {
            assert!(
                blocks.iter().any(|block| block.hash == block2.hash()),
                "invalidated_blocks should reference the hash of block2"
            );

            if blocks.iter().any(|block| block.hash == block2.hash()) {
                Some((height, blocks))
            } else {
                None
            }
        })
        .unwrap();

    match network {
        Network::Mainnet => assert!(
            invalidated_blocks_state_descendants
                .iter()
                .any(|block| block.height == block::Height(653601)),
            "invalidated descendants should contain block3"
        ),
        Network::Testnet(_parameters) => assert!(
            invalidated_blocks_state_descendants
                .iter()
                .any(|block| block.height == block::Height(584001)),
            "invalidated descendants should contain block3"
        ),
    }

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// Invalidating the non-finalized root of a tracked chain previously called
/// `BTreeSet::remove(&chain)`, which compared the stored chain against
/// itself via `Chain::cmp` and reached an `unreachable!()` for matching tip
/// hashes. The root branch now retains by tip hash and the call returns
/// successfully without panicking.
#[test]
fn invalidating_non_finalized_root_does_not_panic() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state
        .commit_new_chain(block1.clone().prepare(), &finalized_state)
        .expect("fake root block should commit to an empty non-finalized state");
    state
        .commit_block(block2.prepare(), &finalized_state)
        .expect("fake child block should extend the fake root chain");

    state
        .invalidate_block(block1.hash())
        .expect("invalidating the chain root should not panic");

    assert!(
        state.best_chain().is_none(),
        "invalidating the root should leave no live non-finalized chain"
    );
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// Sequentially invalidating two same-height sibling fork tips with the same
/// parent previously panicked. The second invalidation produced a shortened
/// parent chain whose tip hash matched an existing entry, and `BTreeSet::insert`
/// reached `Chain::cmp`'s `unreachable!()`. After the fix, `Chain::cmp` returns
/// `Equal` for matching tip hashes, so the duplicate insert is a no-op and the
/// pre-existing parent chain is retained.
#[test]
fn invalidating_same_height_fork_tips_is_idempotent() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2a = block1.make_fake_child().set_work(10);
    let block2b = block1.make_fake_child().set_work(11);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state
        .commit_new_chain(block1.clone().prepare(), &finalized_state)
        .expect("fake root block should commit to an empty non-finalized state");
    state
        .commit_block(block2a.clone().prepare(), &finalized_state)
        .expect("first fork tip should extend the root chain");
    state
        .commit_block(block2b.clone().prepare(), &finalized_state)
        .expect("second fork tip should fork from the root chain");

    state
        .invalidate_block(block2a.hash())
        .expect("first fork tip should invalidate cleanly");
    state
        .invalidate_block(block2b.hash())
        .expect("second sibling fork tip should not panic on collapse to shared parent");

    let best_chain = state
        .best_chain()
        .expect("the parent chain should remain after both sibling tips are invalidated");
    assert!(best_chain.contains_block_hash(block1.hash()));
    assert!(!best_chain.contains_block_hash(block2a.hash()));
    assert!(!best_chain.contains_block_hash(block2b.hash()));
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// `reconsider_block` previously removed the invalidation record from a clone
/// of `invalidated_blocks` rather than from the live map. A second
/// `reconsider_block` for the same hash then replayed the same chain suffix
/// into a chain set that already contained the restored tip, panicking in
/// `Chain::cmp`'s duplicate-tip check. After the fix, the live entry is
/// removed and the second call returns `MissingInvalidatedBlock`.
#[test]
fn reconsider_block_removes_live_entry_and_second_call_returns_missing() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state
        .commit_new_chain(block1.prepare(), &finalized_state)
        .expect("fake root block should commit");
    state
        .commit_block(block2.clone().prepare(), &finalized_state)
        .expect("fake child should extend the root chain");
    state
        .commit_block(block3.prepare(), &finalized_state)
        .expect("fake grandchild should extend the child chain");

    state
        .invalidate_block(block2.hash())
        .expect("invalidating the child should succeed");

    state
        .reconsider_block(block2.hash(), &finalized_state.db)
        .expect("first reconsider should restore the invalidated chain");

    assert!(
        state.invalidated_blocks().values().all(|blocks| {
            blocks
                .first()
                .map(|block| block.hash != block2.hash())
                .unwrap_or(true)
        }),
        "first reconsider should remove the invalidated entry from the live map"
    );

    let second = state.reconsider_block(block2.hash(), &finalized_state.db);
    assert!(
        matches!(second, Err(ReconsiderError::MissingInvalidatedBlock(_))),
        "a second reconsider for the same hash should return MissingInvalidatedBlock; got {second:?}"
    );
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// `reconsider_block` must not destroy the invalidation record when a fallible
/// step fails. We invalidate child `B` (which has a grandchild), then invalidate
/// its parent/root `A`. Calling `reconsider_block(B)` now must fail with
/// `ParentChainNotFound` (because `A` is no longer in the non-finalized state)
/// without removing `B`'s record. A subsequent `reconsider_block(A)` must
/// restore `A`, after which the original `B` record remains available for a
/// later reconsider.
#[test]
fn reconsider_block_preserves_record_when_parent_chain_missing() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state
        .commit_new_chain(block1.clone().prepare(), &finalized_state)
        .expect("fake root should commit");
    state
        .commit_block(block2.clone().prepare(), &finalized_state)
        .expect("fake child should extend the root chain");
    state
        .commit_block(block3.prepare(), &finalized_state)
        .expect("fake grandchild should extend the child chain");

    state
        .invalidate_block(block2.hash())
        .expect("invalidating the child should succeed");
    state
        .invalidate_block(block1.hash())
        .expect("invalidating the root should not panic and should succeed");

    // Now block2's parent (block1) is no longer in the non-finalized state, so
    // reconsidering block2 must fail. The record for block2 must survive the
    // failure so a later reconsider can still restore it.
    let result = state.reconsider_block(block2.hash(), &finalized_state.db);
    assert!(
        matches!(result, Err(ReconsiderError::ParentChainNotFound(_))),
        "reconsider with missing parent chain should fail, got {result:?}"
    );

    assert!(
        state.invalidated_blocks().values().any(|blocks| {
            blocks
                .first()
                .map(|block| block.hash == block2.hash())
                .unwrap_or(false)
        }),
        "a failed reconsider must not destroy the invalidation record"
    );
}

#[test]
fn reconsider_block_and_reconsider_chain_correctly_reconsiders_blocks_and_descendants() -> Result<()>
{
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        reconsider_block_inserts_block_and_descendants_into_chain_for_network(network.clone())?;
    }

    Ok(())
}

fn reconsider_block_inserts_block_and_descendants_into_chain_for_network(
    network: Network,
) -> Result<()> {
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(block3.clone().prepare(), &finalized_state)?;

    assert_eq!(
        state
            .best_chain()
            .unwrap_or(&Arc::new(Chain::default()))
            .blocks
            .len(),
        3
    );

    // Invalidate block2 to update the invalidated_blocks NonFinalizedState
    let _ = state.invalidate_block(block2.hash());

    // Perform checks to ensure the invalidated_block and descendants were added to the invalidated_block
    // state
    let post_invalidated_chain = state.best_chain().unwrap();

    assert_eq!(post_invalidated_chain.blocks.len(), 1);
    assert!(
        post_invalidated_chain.contains_block_hash(block1.hash()),
        "the new modified chain should contain block1"
    );

    assert!(
        !post_invalidated_chain.contains_block_hash(block2.hash()),
        "the new modified chain should not contain block2"
    );
    assert!(
        !post_invalidated_chain.contains_block_hash(block3.hash()),
        "the new modified chain should not contain block3"
    );

    // Reconsider block2 and check that both block2 and block3 were `reconsidered` into the
    // best chain
    state.reconsider_block(block2.hash(), &finalized_state.db)?;

    let best_chain = state.best_chain().unwrap();

    assert!(
        best_chain.contains_block_hash(block2.hash()),
        "the best chain should again contain block2"
    );
    assert!(
        best_chain.contains_block_hash(block3.hash()),
        "the best chain should again contain block3"
    );

    Ok(())
}

#[test]
// This test gives full coverage for `take_chain_if`
fn commit_block_extending_best_chain_doesnt_drop_worst_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(network)?;
    }

    Ok(())
}

fn commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(
    network: Network,
) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child1 = block1.make_fake_child().set_work(1);
    let child2 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    assert_eq!(0, state.chain_set.len());
    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(block2.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(child1.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());
    state.commit_block(child2.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    Ok(())
}

#[test]
fn shorter_chain_can_be_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        shorter_chain_can_be_best_chain_for_network(network)?;
    }
    Ok(())
}

fn shorter_chain_can_be_best_chain_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(Some(2), state.best_chain_len());

    Ok(())
}

#[test]
fn longer_chain_with_more_work_wins() -> Result<()> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        longer_chain_with_more_work_wins_for_network(network)?;
    }

    Ok(())
}

fn longer_chain_with_more_work_wins_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);
    let long_chain_block3 = long_chain_block2.make_fake_child().set_work(1);
    let long_chain_block4 = long_chain_block3.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block3.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block4.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(Some(5), state.best_chain_len());

    Ok(())
}

#[test]
fn equal_length_goes_to_more_work() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        equal_length_goes_to_more_work_for_network(network)?;
    }

    Ok(())
}
fn equal_length_goes_to_more_work_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let less_work_child = block1.make_fake_child().set_work(1);
    let more_work_child = block1.make_fake_child().set_work(3);
    let expected_hash = more_work_child.hash();

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(less_work_child.prepare(), &finalized_state)?;
    state.commit_block(more_work_child.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    let tip_hash = state.best_tip().unwrap().1;
    assert_eq!(expected_hash, tip_hash);

    Ok(())
}

#[test]
fn history_tree_is_updated() -> Result<()> {
    for network in Network::iter() {
        history_tree_is_updated_for_network_upgrade(network, NetworkUpgrade::Heartwood)?;
    }
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
    Ok(())
}

fn history_tree_is_updated_for_network_upgrade(
    network: Network,
    network_upgrade: NetworkUpgrade,
) -> Result<()> {
    let blocks = network.block_map();

    let height = network_upgrade.activation_height(&network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    if network_upgrade == NetworkUpgrade::Heartwood {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_none(),
            "history tree must not exist yet"
        );
    } else {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_some(),
            "history tree must already exist"
        );
    }

    // The Heartwood activation block has an all-zero commitment
    let activation_block = prev_block.make_fake_child().set_block_commitment([0u8; 32]);

    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    assert!(
        chain.history_block_commitment_tree().as_ref().is_some(),
        "history tree must have been (re)created"
    );
    assert_eq!(
        chain
            .history_block_commitment_tree()
            .as_ref()
            .as_ref()
            .unwrap()
            .size(),
        1,
        "history tree must have a single node"
    );

    // To fix the commitment in the next block we must recreate the history tree
    let tree = NonEmptyHistoryTree::from_block(
        &Network::Mainnet,
        activation_block.clone(),
        &chain.sapling_note_commitment_tree_for_tip().root(),
        &chain.orchard_note_commitment_tree_for_tip().root(),
    )
    .unwrap();

    let next_block = activation_block
        .make_fake_child()
        .set_block_commitment(tree.hash().into());

    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();

    assert!(
        state
            .best_chain()
            .unwrap()
            .history_block_commitment_tree()
            .as_ref()
            .is_some(),
        "history tree must still exist"
    );

    Ok(())
}

#[test]
fn commitment_is_validated() {
    for network in Network::iter() {
        commitment_is_validated_for_network_upgrade(network, NetworkUpgrade::Heartwood);
    }
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
}

fn commitment_is_validated_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) {
    let blocks = network.block_map();
    let height = network_upgrade.activation_height(&network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    // The Heartwood activation block must have an all-zero commitment.
    // Test error return when committing the block with the wrong commitment
    let activation_block = prev_block.make_fake_child();
    let err = state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryActivationReserved { .. },
        ) => {},
        _ => panic!("Error must be InvalidBlockCommitment::InvalidChainHistoryActivationReserved instead of {err:?}"),
    };

    // Test committing the Heartwood activation block with the correct commitment
    let activation_block = activation_block.set_block_commitment([0u8; 32]);
    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    // To fix the commitment in the next block we must recreate the history tree
    let chain = state.best_chain().unwrap();
    let tree = NonEmptyHistoryTree::from_block(
        &Network::Mainnet,
        activation_block.clone(),
        &chain.sapling_note_commitment_tree_for_tip().root(),
        &chain.orchard_note_commitment_tree_for_tip().root(),
    )
    .unwrap();

    // Test committing the next block with the wrong commitment
    let next_block = activation_block.make_fake_child();
    let err = state
        .commit_block(next_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryRoot { .. },
        ) => {}
        _ => panic!(
            "Error must be InvalidBlockCommitment::InvalidChainHistoryRoot instead of {err:?}"
        ),
    };

    // Test committing the next block with the correct commitment
    let next_block = next_block.set_block_commitment(tree.hash().into());
    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();
}

#[tokio::test]
async fn non_finalized_state_writes_blocks_to_and_restores_blocks_from_backup_cache() {
    let network = Network::Mainnet;

    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let backup_dir_path = tempfile::Builder::new()
        .prefix("zebra-non-finalized-state-backup-cache")
        .tempdir()
        .expect("temporary directory is created successfully")
        .keep();

    let (mut non_finalized_state, non_finalized_state_sender, _receiver) =
        NonFinalizedState::new(&network)
            .with_backup(
                Some(backup_dir_path.clone()),
                &finalized_state.db,
                false,
                false,
            )
            .await;

    let blocks = network.block_map();
    let height = NetworkUpgrade::Heartwood
        .activation_height(&network)
        .unwrap()
        .0;
    let block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    non_finalized_state
        .commit_new_chain(block.into(), &finalized_state.db)
        .expect("committing test block should succeed");

    non_finalized_state_sender
        .send(non_finalized_state.clone())
        .expect("backup task should have a receiver, channel should be open");

    // Wait for the minimum update time
    tokio::time::sleep(Duration::from_secs(1) + MIN_DURATION_BETWEEN_BACKUP_UPDATES).await;

    let (non_finalized_state, _sender, _receiver) = NonFinalizedState::new(&network)
        .with_backup(Some(backup_dir_path), &finalized_state.db, true, false)
        .await;

    assert_eq!(
        non_finalized_state.best_chain_len(),
        Some(1),
        "non-finalized state should have restored the block committed \
        to the previous non-finalized state"
    );
}

/// Regression test for
/// [GHSA-2gf8-q9rr-jq3h](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-2gf8-q9rr-jq3h).
///
/// `Chain::pop_tip` used to leave entries in `sapling_subtrees` and `orchard_subtrees`,
/// so a chain forked below a subtree boundary inherited stale subtrees from the
/// abandoned branch. Forking should drop any subtree whose `end_height` is above the
/// new tip.
#[test]
fn fork_drops_subtrees_above_fork_point() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut chain = Chain::new(
        &network,
        (block1.coinbase_height().unwrap() - 1).unwrap(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    chain = chain.push(block1.clone().prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(block2.clone().prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(block3.clone().prepare().test_with_zero_spent_utxos())?;

    // Inject a Sapling and an Orchard subtree whose `end_height` is the chain's tip.
    // The block-push pipeline above doesn't complete subtrees on its own for these
    // fixture blocks, so we install them directly via the test-only helpers.
    let tip_height = block3.coinbase_height().unwrap();
    let sapling_node = sapling_crypto::Node::from_bytes([0; 32]).unwrap();
    chain.insert_sapling_subtree(NoteCommitmentSubtree::new(0u16, tip_height, sapling_node));
    let orchard_node = orchard::tree::Node::default();
    chain.insert_orchard_subtree(NoteCommitmentSubtree::new(0u16, tip_height, orchard_node));

    assert_eq!(chain.sapling_subtrees.len(), 1);
    assert_eq!(chain.orchard_subtrees.len(), 1);

    // Fork at `block1`, so `pop_tip` runs twice (for block3 then block2). The
    // subtree at block3's height must be removed; without the fix, it would survive
    // into the forked chain.
    let forked = chain.fork(block1.hash()).expect("block1 is in the chain");

    assert_eq!(forked.non_finalized_tip_hash(), block1.hash());
    assert!(
        forked.sapling_subtrees.is_empty(),
        "fork should have dropped the Sapling subtree completed above the fork point"
    );
    assert!(
        forked.orchard_subtrees.is_empty(),
        "fork should have dropped the Orchard subtree completed above the fork point"
    );

    Ok(())
}

/// Check that the `deferred_pool_balance_change` passed to `with_block_and_spent_utxos`
/// flows through to the resulting block's `chain_value_pool_change`.
#[test]
fn with_block_and_spent_utxos_preserves_deferred_pool_balance_change() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let prepared = SemanticallyVerifiedBlock::from(block);

    let zero_output = transparent::Output {
        value: Amount::zero(),
        lock_script: transparent::Script::new(&[]),
    };
    let zero_utxo = transparent::OrderedUtxo::new(zero_output, Height(1), 1);
    let spent_utxos = prepared
        .block
        .transactions
        .iter()
        .map(AsRef::as_ref)
        .flat_map(Transaction::inputs)
        .flat_map(transparent::Input::outpoint)
        .map(|outpoint| (outpoint, zero_utxo.clone()))
        .collect();

    let expected_deferred = Amount::try_from(123_456_789)?;
    let contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        prepared,
        spent_utxos,
        DeferredPoolBalanceChange::new(expected_deferred),
    )?;

    assert_eq!(
        contextual.chain_value_pool_change.deferred_amount(),
        expected_deferred,
    );

    Ok(())
}

/// Check that after committing a block via `commit_new_chain`, the non-finalized chain's
/// deferred pool amount matches what `calculate_deferred_pool_balance_change` returns for
/// the block's height and network.
#[test]
fn commit_new_chain_sets_chain_value_pools_deferred_amount() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    let block: Arc<Block> = Arc::new(network.test_block(653_599, 583_999).unwrap());
    let height = block.coinbase_height().expect("coinbase height");
    assert!(
        height > network.slow_start_interval(),
        "test block must be past slow_start_interval to exercise the non-trivial branch \
         of calculate_deferred_pool_balance_change",
    );

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state.commit_new_chain(block.prepare(), &finalized_state.db)?;

    let chain = state.best_chain().expect("chain was just committed");
    let expected = calculate_deferred_pool_balance_change(height, &network)
        .value()
        .constrain::<NonNegative>()?;

    assert_eq!(chain.chain_value_pools.deferred_amount(), expected);

    Ok(())
}
