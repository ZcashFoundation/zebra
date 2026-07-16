//! Fixed test vectors for the non-finalized state.

use std::{sync::Arc, time::Duration};

use halo2::pasta::pallas;
use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{self, Block, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::BlockCommitmentTreeRoots,
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
        finalized_state::{calculate_deferred_pool_balance_change, FinalizedState, IntoDisk},
        non_finalized_state::{Chain, NonFinalizedState, MIN_DURATION_BETWEEN_BACKUP_UPDATES},
        ReconsiderError,
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config, SemanticallyVerifiedBlock, ValidateContextError,
};
#[test]
fn construct_empty() {
    let _init_guard = zebra_test::init();
    let _chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
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
        NoteCommitmentTrees::default(),
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
        NoteCommitmentTrees::default(),
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
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    lesser_chain = lesser_chain.push(less_block.prepare().test_with_zero_spent_utxos())?;

    let mut bigger_chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
/// Build an empty `NonFinalizedState` and an ephemeral `FinalizedState` with a
/// populated value pool — the shared setup for the invalidate/reconsider
/// regression tests below.
fn new_invalidate_test_state(network: &Network) -> (NonFinalizedState, FinalizedState) {
    let state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    (state, finalized_state)
}

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

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

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

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

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

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

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

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
    )
    .expect("opening an ephemeral database should succeed");

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
        BlockCommitmentTreeRoots {
            sapling: &chain.sapling_note_commitment_tree_for_tip().root(),
            orchard: &chain.orchard_note_commitment_tree_for_tip().root(),
            ironwood: &chain.ironwood_note_commitment_tree_for_tip().root(),
        },
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
    )
    .expect("opening an ephemeral database should succeed");

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
        BlockCommitmentTreeRoots {
            sapling: &chain.sapling_note_commitment_tree_for_tip().root(),
            orchard: &chain.orchard_note_commitment_tree_for_tip().root(),
            ironwood: &chain.ironwood_note_commitment_tree_for_tip().root(),
        },
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
    )
    .expect("opening an ephemeral database should succeed");

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
        NoteCommitmentTrees::default(),
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
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    state.commit_new_chain(block.prepare(), &finalized_state.db)?;

    let chain = state.best_chain().expect("chain was just committed");
    let expected = calculate_deferred_pool_balance_change(height, &network)
        .value()
        .constrain::<NonNegative>()?;

    assert_eq!(chain.chain_value_pools.deferred_amount(), expected);

    Ok(())
}

/// Builds an empty pre-Heartwood [`NonFinalizedState`], a matching
/// [`FinalizedState`], and a root block whose parent is the empty database's
/// finalized tip, so it can be committed via `commit_checkpoint_block`'s
/// `Chain::new` arm.
///
/// Pre-Heartwood blocks skip the history-tree commitment check (see
/// `block_commitment_is_valid_for_chain_history`).
fn empty_checkpoint_test_states(
    network: &Network,
) -> (NonFinalizedState, FinalizedState, Arc<Block>) {
    let state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("test database opens");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    // Re-root a real pre-Heartwood block onto the empty database's tip so the
    // checkpoint commit's parent-linkage holds.
    let block: Arc<Block> = Arc::new(network.test_block(653_599, 583_999).unwrap());
    let mut root = Block::clone(&block);
    Arc::make_mut(&mut root.header).previous_block_hash = finalized_state.db.finalized_tip_hash();
    let root = Arc::new(root);

    (state, finalized_state, root)
}

/// `commit_checkpoint_block` commits a block whose parent is a non-best
/// chain's tip onto that chain, forking it, instead of assuming a single best
/// chain.
#[test]
fn commit_checkpoint_block_forks_at_parent_tip() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let (mut state, finalized_state, block1) = empty_checkpoint_test_states(&network);
        // Two children of block1 with different work, forming two chains once
        // both are committed on top of block1.
        let block2a = block1.make_fake_child().set_work(10);
        let block2b = block1.make_fake_child().set_work(5);
        // A checkpoint block whose parent is the *lower-work* chain's tip.
        let block3b = block2b.make_fake_child().set_work(5);

        // block1, then a fork: block2a (best) and block2b (side chain).
        state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block1.clone()),
            &finalized_state.db,
        )?;
        state.commit_block(block2a.clone().prepare(), &finalized_state.db)?;
        state.commit_block(block2b.clone().prepare(), &finalized_state.db)?;
        assert_eq!(state.chain_count(), 2, "block1 forked into two chains");

        // A checkpoint block extending the lower-work side chain must commit
        // against block2b's context, not the best chain's tip block2a.
        let (tip_block, finalizable) = state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block3b.clone()),
            &finalized_state.db,
        )?;

        assert_eq!(tip_block.hash, block3b.hash());
        assert_eq!(finalizable.inner_block(), block3b);

        // The block3b chain now exists and contains block3b on top of block2b.
        let chain = state
            .find_chain(|chain| chain.non_finalized_tip_hash() == block3b.hash())
            .expect("the block3b chain exists");
        assert!(chain.contains_block_hash(block2b.hash()));
        assert!(chain.contains_block_hash(block1.hash()));
    }

    Ok(())
}

/// In snapshot-consume (assumeUTXO) mode, `commit_checkpoint_block` resolves
/// spends from memory only (no finalized read) and zeroes the per-block value
/// pool, so a checkpoint block commits without ever reading a (possibly elided)
/// finalized UTXO — the spend-resolution path that previously crashed.
#[test]
fn commit_checkpoint_block_snapshot_consume_skips_finalized_spend_read() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let (mut state, mut finalized_state, block1) = empty_checkpoint_test_states(&network);

        // Enable snapshot-consume mode with an empty survivor set (every output
        // is a non-survivor → maximal elision), and UTXO-byte elision on.
        finalized_state.db.set_snapshot_consume(Some(Arc::new(
            crate::snapshot_consume::SnapshotConsumeState::from_parts(
                network.clone(),
                Height(1_000_000),
                true,
                Some(Arc::new(
                    crate::snapshot_consume::SurvivorSet::from_bytes(Vec::new())
                        .expect("empty survivor set loads"),
                )),
            ),
        )));

        // A short pinned chain of checkpoint blocks commits cleanly: the
        // in-memory commit never reads the finalized UTXO set, and the value-pool
        // computation is bypassed (it can't error).
        let (_tip, _finalizable) = state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block1.clone()),
            &finalized_state.db,
        )?;

        let block2 = block1.make_fake_child().set_work(10);
        let (tip_block, finalizable) = state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block2.clone()),
            &finalized_state.db,
        )?;

        assert_eq!(tip_block.hash, block2.hash());
        assert_eq!(finalizable.inner_block(), block2);
        assert_eq!(state.chain_count(), 1, "the pinned chain extends cleanly");
    }

    Ok(())
}

/// Enables snapshot-consume mode on `finalized_state`, so the supplied-tree
/// commit path is taken (no survivor set is needed to use supplied trees).
fn enable_snapshot_consume(finalized_state: &mut FinalizedState, network: &Network) {
    finalized_state.db.set_snapshot_consume(Some(Arc::new(
        crate::snapshot_consume::SnapshotConsumeState::from_parts(
            network.clone(),
            Height(1_000_000),
            true,
            None,
        ),
    )));
}

/// Builds a Sapling-era (pre-Heartwood, pre-Orchard) checkpoint root block on an
/// empty database, with its `hashFinalSaplingRoot` set to `sapling_root` so the
/// supplied-tree path verifies against the header (the era where the header pins
/// the bare Sapling root). Returns the empty states and the root block.
fn sapling_era_checkpoint_states(
    network: &Network,
    sapling_root: [u8; 32],
) -> (NonFinalizedState, FinalizedState, Arc<Block>) {
    let (state, finalized_state, root) = empty_checkpoint_test_states(network);
    let root = root.set_block_commitment(sapling_root);
    (state, finalized_state, root)
}

/// Returns the sapling/orchard note commitment trees a folded checkpoint commit
/// of `root` produces, by committing it normally (folding) to a throwaway state.
fn folded_trees_for(
    network: &Network,
    root: &Arc<Block>,
) -> zebra_chain::parallel::tree::NoteCommitmentTrees {
    let (mut state, finalized_state, _) = empty_checkpoint_test_states(network);
    let (_tip, finalizable) = state
        .commit_checkpoint_block(
            CheckpointVerifiedBlock::from(root.clone()),
            &finalized_state.db,
        )
        .expect("the folded baseline commit succeeds");

    match finalizable {
        crate::request::FinalizableBlock::Contextual { treestate, .. } => {
            treestate.note_commitment_trees
        }
        _ => panic!("commit_checkpoint_block returns a Contextual finalizable block"),
    }
}

/// Builds a non-trivial orchard note commitment tree with `count` distinct notes
/// and its cached root, for supplied-tree tests. Orchard notes are plain pallas
/// base elements, so this is cheap and deterministic without real shielded data.
fn orchard_tree_with_notes(count: u64) -> Arc<orchard::tree::NoteCommitmentTree> {
    let mut tree = orchard::tree::NoteCommitmentTree::default();
    for i in 0..count {
        tree.append(pallas::Base::from(i + 1))
            .expect("appending a note to the orchard tree succeeds");
    }
    let _ = tree.root();
    Arc::new(tree)
}

/// In snapshot-consume mode, a supplied, verified note commitment tree is written
/// **directly** instead of folding: the committed sapling/orchard trees are
/// byte-identical to the supplied ones (including their cached roots and
/// serialized frontiers), and the supplied frontier is used. This is the
/// throughput win of the P2P snapshot feature (finding #10).
///
/// The test block has no orchard notes of its own, so the only way the committed
/// orchard tree can be the non-trivial supplied tree is if the supplied path was
/// taken — making this a write-through *and* a fold-skip check with a non-trivial
/// tree. The sapling tree is supplied empty (matching the pre-Sapling-output test
/// block's header) to exercise the header-pin verification on the accept path.
#[test]
fn commit_checkpoint_block_uses_supplied_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // The empty sapling tree the test block folds to; the header pins its root, so
    // the supplied sapling tree verifies on the accept path.
    let (_, _, base_root) = empty_checkpoint_test_states(&network);
    let folded = folded_trees_for(&network, &base_root);
    let sapling_root: [u8; 32] = (&folded.sapling.root()).into();

    // A non-trivial orchard frontier (10 notes) to be written through directly.
    let supplied_orchard = orchard_tree_with_notes(10);
    let supplied = zebra_chain::parallel::tree::NoteCommitmentTrees {
        sapling: folded.sapling.clone(),
        orchard: supplied_orchard.clone(),
        ..Default::default()
    };
    // The serialized frontier we expect to round-trip onto disk byte-for-byte.
    let supplied_orchard_bytes = supplied_orchard.as_bytes();

    let (mut state, mut finalized_state, root) =
        sapling_era_checkpoint_states(&network, sapling_root);
    enable_snapshot_consume(&mut finalized_state, &network);

    let (_tip, finalizable) = state.commit_checkpoint_block(
        CheckpointVerifiedBlock::from(root.clone()).with_supplied_trees(Some(supplied)),
        &finalized_state.db,
    )?;

    let committed = match finalizable {
        crate::request::FinalizableBlock::Contextual { treestate, .. } => {
            treestate.note_commitment_trees
        }
        _ => panic!("expected a Contextual finalizable block"),
    };

    // The committed sapling tree matches the verified supplied (empty) one, and the
    // committed orchard tree is the non-trivial supplied frontier written directly:
    // root, full tree equality, and serialized bytes all match.
    assert_eq!(
        committed.sapling, folded.sapling,
        "supplied sapling written"
    );
    assert_eq!(
        committed.orchard, supplied_orchard,
        "the non-trivial supplied orchard frontier was written through (fold skipped)",
    );
    assert_eq!(
        committed.orchard.root(),
        supplied_orchard.root(),
        "committed orchard root matches the supplied frontier",
    );
    assert_eq!(
        committed.orchard.as_bytes(),
        supplied_orchard_bytes,
        "committed orchard frontier serializes byte-identically to the supplied one",
    );

    Ok(())
}

/// Deterministic proof that the per-note fold is **skipped** when a verified tree
/// is supplied: supply a sapling tree matching the header but a *deliberately
/// wrong* (non-empty) orchard tree. If the fold ran, the committed orchard tree
/// would be the correct (empty, pre-Orchard) folded tree; because the supplied
/// path is taken, the committed orchard tree is the wrong supplied one.
#[test]
fn commit_checkpoint_block_supplied_trees_skip_the_fold() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let (_, _, base_root) = empty_checkpoint_test_states(&network);
    let folded = folded_trees_for(&network, &base_root);
    let sapling_root: [u8; 32] = (&folded.sapling.root()).into();

    // A "poison" orchard tree: non-empty, so it differs from the correct
    // (pre-Orchard, empty) folded orchard tree. Its only purpose is to be
    // detectable in the committed result.
    let mut poison_orchard = orchard::tree::NoteCommitmentTree::default();
    poison_orchard
        .append(pallas_marker_note())
        .expect("appending one note to an empty orchard tree succeeds");
    let _ = poison_orchard.root();
    let poison_orchard = Arc::new(poison_orchard);
    assert_ne!(
        poison_orchard, folded.orchard,
        "the poison orchard tree must differ from the correct folded one",
    );

    let supplied = zebra_chain::parallel::tree::NoteCommitmentTrees {
        sapling: folded.sapling.clone(),
        orchard: poison_orchard.clone(),
        ..Default::default()
    };

    let (mut state, mut finalized_state, root) =
        sapling_era_checkpoint_states(&network, sapling_root);
    enable_snapshot_consume(&mut finalized_state, &network);

    let (_tip, finalizable) = state.commit_checkpoint_block(
        CheckpointVerifiedBlock::from(root.clone()).with_supplied_trees(Some(supplied)),
        &finalized_state.db,
    )?;

    let committed = match finalizable {
        crate::request::FinalizableBlock::Contextual { treestate, .. } => {
            treestate.note_commitment_trees
        }
        _ => panic!("expected a Contextual finalizable block"),
    };

    assert_eq!(
        committed.orchard, poison_orchard,
        "the supplied (poison) orchard tree was written directly: the fold was skipped",
    );

    Ok(())
}

/// When a supplied tree would complete a `2^16`-leaf note-commitment subtree, the
/// commit **falls back to folding the whole block** (the supplied frontier blob
/// cannot reproduce a mid-block subtree completion, and subtree roots are served +
/// RPC-checked, so they must stay byte-identical to a normally-synced node).
///
/// Proof: supply an orchard tree that completes subtree 0 (its position is exactly
/// `2^16 - 1`) while the tip orchard tree is empty. If the supplied path were
/// (incorrectly) taken, the committed orchard tree would be the supplied
/// `2^16`-note tree; because the subtree completion forces a fold, the committed
/// orchard tree is the *folded* (empty, for this note-less block) one instead.
#[test]
fn commit_checkpoint_block_subtree_completion_falls_back_to_folding() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let (_, _, base_root) = empty_checkpoint_test_states(&network);
    let folded = folded_trees_for(&network, &base_root);
    let sapling_root: [u8; 32] = (&folded.sapling.root()).into();

    // A supplied orchard tree with exactly 2^16 notes: its last leaf completes
    // subtree 0, so `contains_new_subtree` against the empty tip tree is true.
    let subtree_size: u64 = 1 << u64::from(zebra_chain::subtree::TRACKED_SUBTREE_HEIGHT);
    let completing_orchard = orchard_tree_with_notes(subtree_size);
    assert!(
        completing_orchard.is_complete_subtree(),
        "the supplied orchard tree must complete a subtree to exercise the fallback",
    );
    assert!(
        completing_orchard.contains_new_subtree(&orchard::tree::NoteCommitmentTree::default()),
        "the supplied orchard tree must register as a new subtree vs the empty tip",
    );

    let supplied = zebra_chain::parallel::tree::NoteCommitmentTrees {
        sapling: folded.sapling.clone(),
        orchard: completing_orchard.clone(),
        ..Default::default()
    };

    let (mut state, mut finalized_state, root) =
        sapling_era_checkpoint_states(&network, sapling_root);
    enable_snapshot_consume(&mut finalized_state, &network);

    let (_tip, finalizable) = state.commit_checkpoint_block(
        CheckpointVerifiedBlock::from(root.clone()).with_supplied_trees(Some(supplied)),
        &finalized_state.db,
    )?;

    let committed = match finalizable {
        crate::request::FinalizableBlock::Contextual { treestate, .. } => {
            treestate.note_commitment_trees
        }
        _ => panic!("expected a Contextual finalizable block"),
    };

    // The block has no orchard notes, so folding yields the empty tip tree — NOT
    // the supplied subtree-completing tree. This proves the fold ran.
    assert_eq!(
        committed.orchard, folded.orchard,
        "a subtree-completing supplied tree forces a full fold (folded == empty)",
    );
    assert_ne!(
        committed.orchard, completing_orchard,
        "the subtree-completing supplied tree was NOT written directly",
    );

    Ok(())
}

/// Without snapshot-consume mode enabled, supplied trees are **ignored** and the
/// block folds normally — the throughput path is strictly gated so a normal sync
/// is unaffected.
#[test]
fn supplied_trees_are_ignored_outside_snapshot_consume() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let (_, _, base_root) = empty_checkpoint_test_states(&network);
    let folded = folded_trees_for(&network, &base_root);
    let sapling_root: [u8; 32] = (&folded.sapling.root()).into();

    // A poison orchard tree as above. With snapshot-consume mode OFF, it must be
    // ignored: the committed orchard tree is the correct folded (empty) one.
    let mut poison_orchard = orchard::tree::NoteCommitmentTree::default();
    poison_orchard
        .append(pallas_marker_note())
        .expect("appending one note succeeds");
    let _ = poison_orchard.root();
    let poison_orchard = Arc::new(poison_orchard);

    let supplied = zebra_chain::parallel::tree::NoteCommitmentTrees {
        sapling: folded.sapling.clone(),
        orchard: poison_orchard,
        ..Default::default()
    };

    // snapshot-consume mode NOT enabled on this finalized state.
    let (mut state, finalized_state, root) = sapling_era_checkpoint_states(&network, sapling_root);

    let (_tip, finalizable) = state.commit_checkpoint_block(
        CheckpointVerifiedBlock::from(root.clone()).with_supplied_trees(Some(supplied)),
        &finalized_state.db,
    )?;

    let committed = match finalizable {
        crate::request::FinalizableBlock::Contextual { treestate, .. } => {
            treestate.note_commitment_trees
        }
        _ => panic!("expected a Contextual finalizable block"),
    };

    assert_eq!(
        committed.orchard, folded.orchard,
        "supplied trees are ignored outside snapshot-consume mode: the block folds",
    );

    Ok(())
}

/// A supplied sapling tree whose root contradicts the block header's
/// `hashFinalSaplingRoot` is rejected with a fatal `SuppliedSaplingTreeRootMismatch`
/// (the prior fix's verification, preserved on the in-memory commit path) — never
/// silently accepted.
#[test]
fn commit_checkpoint_block_rejects_unverifiable_supplied_sapling_tree() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let (_, _, base_root) = empty_checkpoint_test_states(&network);
    let folded = folded_trees_for(&network, &base_root);
    let correct_sapling_root: [u8; 32] = (&folded.sapling.root()).into();

    // Pin a DIFFERENT sapling root in the header than the supplied tree's root.
    let mut wrong_root = correct_sapling_root;
    wrong_root[0] ^= 0xff;

    let (mut state, mut finalized_state, root) =
        sapling_era_checkpoint_states(&network, wrong_root);
    enable_snapshot_consume(&mut finalized_state, &network);

    let error = state
        .commit_checkpoint_block(
            CheckpointVerifiedBlock::from(root.clone()).with_supplied_trees(Some(folded.clone())),
            &finalized_state.db,
        )
        .err();

    assert!(
        matches!(
            error,
            Some(ValidateContextError::SuppliedSaplingTreeRootMismatch { .. })
        ),
        "expected SuppliedSaplingTreeRootMismatch, got {error:?}",
    );

    Ok(())
}

/// A small non-zero orchard note commitment, for building a "poison" orchard tree
/// that is detectably different from the empty (pre-Orchard) folded tree.
fn pallas_marker_note() -> pallas::Base {
    // Any fixed non-zero pallas base element works as a distinguishable note.
    pallas::Base::from(1u64)
}

/// A failed `commit_checkpoint_block` leaves the non-finalized state exactly
/// as it was, so the write worker can treat the error as non-fatal.
#[test]
fn commit_checkpoint_block_error_leaves_state_untouched() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let (mut state, finalized_state, block1) = empty_checkpoint_test_states(&network);
        let block2 = block1.make_fake_child().set_work(10);

        state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block1.clone()),
            &finalized_state.db,
        )?;
        state.commit_checkpoint_block(
            CheckpointVerifiedBlock::from(block2.clone()),
            &finalized_state.db,
        )?;

        let before = state.clone();

        // A block whose parent matches no chain tip and isn't the finalized
        // tip is rejected without mutating the state.
        let orphan = block1.make_fake_child().make_fake_child().set_work(10);
        let error = state
            .commit_checkpoint_block(CheckpointVerifiedBlock::from(orphan), &finalized_state.db)
            .err();

        assert!(
            matches!(error, Some(ValidateContextError::NotReadyToBeCommitted)),
            "expected NotReadyToBeCommitted, got {error:?}",
        );
        assert!(
            before.eq_internal_state(&state),
            "the non-finalized state must be unchanged after a failed commit",
        );
    }

    Ok(())
}

/// `finalize_root(Some(best_root_hash))` is equivalent to `finalize()` when a
/// single chain exists.
#[test]
fn finalize_root_some_matches_finalize_single_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let (mut by_hash_state, finalized_state, block1) = empty_checkpoint_test_states(&network);
        let block2 = block1.make_fake_child().set_work(10);

        by_hash_state.commit_new_chain(block1.clone().prepare(), &finalized_state.db)?;
        by_hash_state.commit_block(block2.clone().prepare(), &finalized_state.db)?;

        let mut best_state = by_hash_state.clone();

        let root_hash = by_hash_state
            .best_chain()
            .expect("chain committed")
            .non_finalized_root_hash();

        let by_hash = by_hash_state.finalize_root(Some(root_hash)).inner_block();
        let best = best_state.finalize().inner_block();

        assert_eq!(
            by_hash, best,
            "finalize_root(Some(best)) must equal finalize()"
        );
        assert_eq!(by_hash, block1);
        assert!(
            best_state.eq_internal_state(&by_hash_state),
            "both states must be left identical",
        );
    }

    Ok(())
}

/// `finalize_root(Some(hash))` finalizes the named chain's root and drops a
/// sibling chain whose root differs, even when the sibling has more work.
#[test]
fn finalize_root_drops_mismatched_sibling() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let (mut state, finalized_state, block1) = empty_checkpoint_test_states(&network);
        // A fork on top of block1: two children at the same height.
        let block2a = block1.make_fake_child().set_work(10);
        let block2b = block1.make_fake_child().set_work(20);

        state.commit_new_chain(block1.clone().prepare(), &finalized_state.db)?;
        state.commit_block(block2a.clone().prepare(), &finalized_state.db)?;
        state.commit_block(block2b.clone().prepare(), &finalized_state.db)?;
        assert_eq!(state.chain_count(), 2, "block1 forked into two chains");

        // Finalize the shared root block1: both chains keep going, now rooted
        // at block2a and block2b respectively.
        let finalized = state.finalize_root(Some(block1.hash())).inner_block();
        assert_eq!(finalized, block1);
        assert_eq!(
            state.chain_count(),
            2,
            "both forks survive the shared-root pop"
        );

        // Now finalize block2a by hash: the block2b chain is rooted at block2b
        // (a different hash), so it forked below block2a and is dropped, even
        // though block2b has more work.
        let finalized = state.finalize_root(Some(block2a.hash())).inner_block();
        assert_eq!(finalized, block2a);
        assert!(
            state.best_chain().is_none(),
            "block2a finalized, block2b chain dropped as a mismatched sibling",
        );
    }

    Ok(())
}
