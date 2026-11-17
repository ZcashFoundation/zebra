//! Fixed test vectors for the non-finalized state.

use std::{sync::Arc, time::Duration};

use zebra_chain::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block::{self, Block, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::BlockCommitmentTreeRoots,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
    transparent,
    value_balance::ValueBalance,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    error::PreciousError,
    service::{
        finalized_state::{calculate_deferred_pool_balance_change, FinalizedState},
        non_finalized_state::{Chain, NonFinalizedState, MIN_DURATION_BETWEEN_BACKUP_UPDATES},
        InvalidateError, ReconsiderError,
    },
    tests::FakeChainHelper,
    Config,
};

/// Build value-conserving transfers with complete spent-output maps, including intra-block spends.
/// This exercises chain indexing, not proof-of-work, script, or subsidy verification.
fn large_transfer_chain(
    destinations: &[&[usize]],
) -> Result<(
    Chain,
    Vec<crate::ContextuallyVerifiedBlock>,
    [transparent::Address; 2],
    Amount<NonNegative>,
)> {
    use std::collections::HashMap;
    use zebra_chain::{
        amount::{DeferredPoolBalanceChange, MAX_MONEY},
        parameters::NetworkKind,
        transaction::{self, LockTime, Transaction},
    };

    let network = Network::Mainnet;
    let finalized_height = Height(600_000);
    let value: Amount<NonNegative> = (MAX_MONEY / 2 + 1).try_into()?;
    let pools = ValueBalance::from_transparent_amount(value);
    let addresses = [
        transparent::Address::from_script_hash(NetworkKind::Mainnet, [0x41; 20]),
        transparent::Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]),
    ];
    let chain = Chain::new(
        &network,
        finalized_height,
        NoteCommitmentTrees::default(),
        Default::default(),
        pools,
    );
    let mut outpoint = transparent::OutPoint {
        hash: transaction::Hash([0x11; 32]),
        index: 0,
    };
    let mut utxo = transparent::OrderedUtxo::new(
        transparent::Output::new(value, addresses[0].script()),
        finalized_height,
        1,
    );
    let mut parent = block::Hash([0x22; 32]);
    let mut blocks = Vec::new();
    for (block_index, destinations) in destinations.iter().enumerate() {
        let height = (finalized_height + i64::try_from(block_index + 1)?)
            .expect("the fixture heights are below the maximum");
        let mut transactions = vec![Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output::new(
                Amount::zero(),
                transparent::Script::new(&[]),
            )],
            LockTime::unlocked(),
            Height(0),
        ))];
        let mut spent = HashMap::new();
        for &destination in *destinations {
            let transaction = Arc::new(Transaction::test_v4(
                vec![transparent::Input::PrevOut {
                    outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: u32::MAX,
                }],
                vec![transparent::Output::new(
                    value,
                    addresses[destination].script(),
                )],
                LockTime::unlocked(),
                Height(0),
            ));
            assert!(spent.insert(outpoint, utxo).is_none());
            outpoint = transparent::OutPoint {
                hash: transaction.hash(),
                index: 0,
            };
            utxo = transparent::OrderedUtxo::new(
                transaction.outputs()[0].clone(),
                height,
                transactions.len(),
            );
            transactions.push(transaction);
        }
        let mut header: block::Header =
            zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
        header.previous_block_hash = parent;
        header.merkle_root = transactions
            .iter()
            .map(|transaction| transaction.hash())
            .collect();
        let block = Arc::new(Block {
            header: Arc::new(header),
            transactions,
        })
        .set_work(10);
        assert_eq!(
            block.transaction_fees(&spent)?,
            Amount::<NonNegative>::zero()
        );
        let contextual = crate::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            block.prepare(),
            spent,
            DeferredPoolBalanceChange::zero(),
            &network,
            pools,
        )?;
        assert_eq!(contextual.chain_value_pool_change, ValueBalance::zero());
        parent = contextual.hash;
        blocks.push(contextual);
    }
    Ok((chain, blocks, addresses, value))
}

#[test]
fn large_self_transfers_preserve_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[1], &[1, 1]])?;
    let root = chain.push(blocks[0].clone())?;
    let chain = root.clone().push(blocks[1].clone())?;
    let selected = HashSet::from([addresses[1]]);
    assert_eq!(
        chain.partial_transparent_balance_change(&selected).0,
        value.constrain::<NegativeAllowed>()?
    );
    // Only the output that the chain doesn't spend again remains.
    let (created, spent) =
        chain.partial_transparent_utxo_changes(&selected, &(Height(0)..=Height::MAX), None);
    assert_eq!(created.len(), 1);
    assert_eq!(spent.len(), 2);
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(blocks[1].clone())?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn reverting_intra_block_transfers_preserves_non_finalized_indexes() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (chain, blocks, _, _) = large_transfer_chain(&[&[1], &[0, 1]])?;
    let root = chain.push(blocks[0].clone())?;
    let chain = root.clone().push(blocks[1].clone())?;
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(blocks[1].clone())?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn finalizing_self_transfers_preserves_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[0, 0], &[1]])?;
    let mut chain = chain.push(blocks[0].clone())?.push(blocks[1].clone())?;
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, blocks[0].hash);
    let selected = HashSet::from([addresses[0]]);
    assert_eq!(
        chain.partial_transparent_balance_change(&selected).0,
        (Amount::<NegativeAllowed>::zero() - value.constrain::<NegativeAllowed>()?)?
    );
    let (created, spent) =
        chain.partial_transparent_utxo_changes(&selected, &(Height(0)..=Height::MAX), None);
    assert!(created.is_empty());
    assert_eq!(spent.len(), 1);
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, blocks[1].hash);
    // The balance change needs a non-finalized root, so check the emptied index directly.
    assert!(chain.is_empty());
    assert!(chain.transparent_transfers_by_height.is_empty());
    Ok(())
}

#[test]
fn coinbase_fee_transfer_preserves_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;
    use zebra_chain::amount::DeferredPoolBalanceChange;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[1], &[1]])?;
    let root = chain.push(blocks[0].clone())?;
    let mut block = blocks[1].block.as_ref().clone();
    block.transactions[0] = Arc::new(
        block.transactions[0]
            .as_ref()
            .clone()
            .with_transparent_outputs(vec![transparent::Output::new(value, addresses[1].script())]),
    );
    block.transactions[1] = Arc::new(
        block.transactions[1]
            .as_ref()
            .clone()
            .with_transparent_outputs(vec![transparent::Output::new(
                Amount::zero(),
                transparent::Script::new(&[]),
            )]),
    );
    Arc::make_mut(&mut block.header).merkle_root = block
        .transactions
        .iter()
        .map(|transaction| transaction.hash())
        .collect();
    assert_eq!(block.transaction_fees(&blocks[1].spent_outputs)?, value);
    let block = crate::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        Arc::new(block).prepare(),
        blocks[1].spent_outputs.as_ref().clone(),
        DeferredPoolBalanceChange::zero(),
        &Network::Mainnet,
        root.chain_value_pools,
    )?;
    assert_eq!(block.chain_value_pool_change, ValueBalance::zero());
    let chain = root.clone().push(block.clone())?;
    assert_eq!(
        chain
            .partial_transparent_balance_change(&HashSet::from([addresses[1]]))
            .0,
        value.constrain::<NegativeAllowed>()?
    );
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(block)?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn contextual_maps_preserve_intra_block_spends() -> Result<()> {
    use crate::{ContextuallyVerifiedBlock, SemanticallyVerifiedBlock};
    use std::collections::HashMap;
    use zebra_chain::{
        amount::DeferredPoolBalanceChange,
        parameters::testnet::ConfiguredActivationHeights,
        transaction::{LockTime, Transaction},
    };

    let height = Height(1_000);
    let output = |value: u64| {
        transparent::Output::new(
            value.try_into().unwrap(),
            transparent::Script::new(&[0; 1024]),
        )
    };
    let coinbase = Arc::new(Transaction::test_v1(
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![output(10)],
        LockTime::unlocked(),
    ));
    let mut transactions = vec![coinbase];
    let mut spent = HashMap::new();
    for value in [9, 8] {
        let previous = transactions.last().unwrap();
        let outpoint = transparent::OutPoint {
            hash: previous.hash(),
            index: 0,
        };
        spent.insert(
            outpoint,
            transparent::OrderedUtxo::new(
                previous.outputs()[0].clone(),
                height,
                transactions.len() - 1,
            ),
        );
        transactions.push(Arc::new(Transaction::test_v1(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            vec![output(value)],
            LockTime::unlocked(),
        )));
    }
    let block = Arc::new(Block {
        header: Arc::new(zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?),
        transactions,
    });
    let plain = transparent::utxos_from_ordered_utxos(spent.clone());
    let nu7 = Network::new_regtest(
        ConfiguredActivationHeights {
            nu7: Some(height.0),
            ..Default::default()
        }
        .into(),
    );
    for network in [Network::Mainnet, nu7] {
        let expected = block.chain_value_pool_change_and_fees(
            &plain,
            DeferredPoolBalanceChange::zero(),
            &network,
            ValueBalance::zero(),
        )?;
        let (contextual, fees) = ContextuallyVerifiedBlock::with_block_spent_utxos_and_fees(
            block.clone().prepare(),
            spent.clone(),
            DeferredPoolBalanceChange::zero(),
            &network,
            ValueBalance::zero(),
        )?;
        assert_eq!((contextual.chain_value_pool_change, fees), expected);
        assert_eq!(contextual.spent_outputs.as_ref(), &spent);
        assert_eq!(block.transaction_fees(&spent)?.zatoshis(), 2);
        let cloned = contextual.clone();
        assert_eq!(cloned, contextual);
        assert!(Arc::ptr_eq(&contextual.new_outputs, &cloned.new_outputs));
        assert!(Arc::ptr_eq(
            &contextual.spent_outputs,
            &cloned.spent_outputs
        ));
        let semantic = SemanticallyVerifiedBlock::from(cloned);
        let unshared = |outputs: &std::collections::HashMap<
            transparent::OutPoint,
            Arc<transparent::OrderedUtxo>,
        >| {
            outputs
                .iter()
                .map(|(outpoint, utxo)| (*outpoint, utxo.as_ref().clone()))
                .collect::<std::collections::HashMap<_, _>>()
        };
        assert_eq!(*semantic.new_outputs, unshared(&contextual.new_outputs));
        let unique = contextual;
        let expected_outputs = unshared(&unique.new_outputs);
        assert_eq!(Arc::strong_count(&unique.new_outputs), 1);
        assert_eq!(
            *SemanticallyVerifiedBlock::from(unique).new_outputs,
            expected_outputs
        );
    }
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network)
        .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    (state, finalized_state)
}

#[test]
fn cached_proposal_respects_block_and_parent_invalidation() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let parent = Arc::new(network.test_block(653599, 583999).unwrap());
    let mut solved = parent.make_fake_child().set_work(10);
    Arc::make_mut(&mut Arc::make_mut(&mut solved).header).nonce = [1; 32].into();
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(parent.clone().prepare(), &finalized_state)
        .unwrap();

    // Retain the successful proposal payload before committing its solved counterpart.
    let mut proposal = (*solved).clone();
    Arc::make_mut(&mut proposal.header).nonce = [0; 32].into();
    let mut proposal_state = state.clone();
    proposal_state.disable_metrics();
    proposal_state
        .commit_block(Arc::new(proposal).prepare(), &finalized_state)
        .unwrap();
    let mut cached = proposal_state.best_tip_block().unwrap().clone();
    assert_ne!(cached.hash, solved.hash());
    cached.rebind(solved.clone(), std::time::Instant::now());
    state
        .commit_block(solved.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(solved.hash()).unwrap();

    let (committed, _commits) = tokio::sync::mpsc::channel(1);
    let (validated, bodies) = tokio::sync::watch::channel(None);
    let notifications = crate::MinedBlockNotifications::new(committed, validated);

    let normal_error = state
        .commit_block(solved.clone().prepare(), &finalized_state)
        .unwrap_err();
    assert_eq!(
        normal_error,
        crate::ValidateContextError::BlockPreviouslyInvalidated {
            block_hash: solved.hash(),
        }
    );
    assert_eq!(
        state.commit_prevalidated_contextual(
            cached.clone(),
            &finalized_state,
            Some(&notifications),
        ),
        Err(normal_error),
    );
    assert_eq!(state.best_tip().unwrap().1, parent.hash());
    assert!(!state.any_chain_contains(&solved.hash()));
    assert!(bodies.borrow().is_none());

    // A different solved hash is not itself invalidated, but cannot restore its removed parent.
    let mut sibling = (*solved).clone();
    Arc::make_mut(&mut sibling.header).nonce = [2; 32].into();
    let sibling = Arc::new(sibling);
    cached.rebind(sibling.clone(), std::time::Instant::now());
    // Keep another root live: invalidating the only non-finalized root is rejected.
    let other_root = parent.clone().set_work(20);
    state
        .commit_new_chain(other_root.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(parent.hash()).unwrap();
    let normal_error = state
        .commit_block(sibling.prepare(), &finalized_state)
        .unwrap_err();
    assert_eq!(
        normal_error,
        crate::ValidateContextError::NotReadyToBeCommitted
    );
    assert_eq!(
        state.commit_prevalidated_contextual(cached, &finalized_state, Some(&notifications)),
        Err(normal_error),
    );
    assert_eq!(state.best_tip().unwrap().1, other_root.hash());
    assert!(!state.any_chain_contains(&parent.hash()));
    assert!(bodies.borrow().is_none());
}

#[test]
fn compact_header_screening_respects_local_invalidation() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let parent = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = parent.make_fake_child();
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(parent.prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(child.hash()).unwrap();
    assert_eq!(
        crate::service::read::difficulty::candidate_header_context(
            &state,
            &finalized_state.db,
            &network,
            &child.header,
            child.coinbase_height().unwrap(),
            true,
        )
        .unwrap_err(),
        crate::ValidateContextError::BlockPreviouslyInvalidated {
            block_hash: child.hash()
        },
    );
}

/// Reject invalidating the only root before changing either live chains or invalidation records.
#[test]
fn invalidating_only_non_finalized_root_preserves_state() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = root.make_fake_child().set_work(10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    // Keep an existing invalidation record to check that rejection preserves it too.
    let grandchild = child.make_fake_child().set_work(1);
    state
        .commit_block(grandchild.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(grandchild.hash()).unwrap();
    let before = state.clone();

    let result = state.invalidate_block(root.hash());
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    assert!(state.eq_internal_state(&before));
    assert_eq!(
        state.invalidated_blocks.len(),
        before.invalidated_blocks.len()
    );
    for (height, blocks) in &before.invalidated_blocks {
        assert!(Arc::ptr_eq(
            blocks,
            state.invalidated_blocks.get(height).unwrap()
        ));
    }
}

/// A root can still be invalidated when another chain remains available.
#[test]
fn invalidating_non_finalized_root_preserves_other_chain() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let other_root = root.clone().set_work(20);
    let child = root.make_fake_child().set_work(10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_new_chain(other_root.clone().prepare(), &finalized_state)
        .unwrap();

    assert_eq!(state.invalidate_block(root.hash()).unwrap(), root.hash());
    assert_eq!(state.chain_count(), 1);
    assert_eq!(state.best_tip_block().unwrap().hash, other_root.hash());
    assert!(!state.any_chain_contains(&root.hash()));
    assert!(!state.any_chain_contains(&child.hash()));
    assert_eq!(state.invalidated_blocks.values().next().unwrap().len(), 2);
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

    // Keep another root live so invalidating block1 does not empty the state.
    state
        .commit_new_chain(block1.clone().set_work(20).prepare(), &finalized_state)
        .expect("independent root should commit");

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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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
    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
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

    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
        .expect("opening an ephemeral database should succeed");

    let backup_dir = tempfile::Builder::new()
        .prefix("zebra-non-finalized-state-backup-cache")
        .tempdir()
        .expect("temporary directory is created successfully");
    let backup_dir_path = backup_dir.path().to_path_buf();

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

    // Poll restoration until the rate-limited writer has finished writing a readable backup.
    let non_finalized_state = tokio::time::timeout(
        MIN_DURATION_BETWEEN_BACKUP_UPDATES + Duration::from_secs(30),
        async {
            loop {
                let (restored_state, _sender, _receiver) = NonFinalizedState::new(&network)
                    .with_backup(
                        Some(backup_dir_path.clone()),
                        &finalized_state.db,
                        true,
                        // Only the original state should write backups while restoration is polled.
                        true,
                    )
                    .await;
                if restored_state.best_chain_len().is_some() {
                    break restored_state;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )
    .await
    .expect("the backup task should write a restorable non-finalized block");

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

/// From the NSM reissuance height, the deferred (lockbox) funding stream is a
/// fraction of the whole block subsidy, including the additional subsidy that depends on the parent
/// block's chain value pools.
#[test]
fn deferred_pool_balance_change_uses_nsm_subsidy() -> Result<()> {
    use zebra_chain::parameters::{
        subsidy::{
            block_subsidy_with_parent_pools, funding_stream_values, scheduled_block_subsidy,
            FundingStreamReceiver,
        },
        testnet::{
            ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams, RegtestParameters,
        },
    };

    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(10),
            ..Default::default()
        },
        funding_streams: Some(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Deferred,
                numerator: 12,
                addresses: None,
            }]),
        }]),
        nsm_reissuance_height: Some(Height(10)),
        ..Default::default()
    });

    let deferred = |height, subsidy| {
        funding_stream_values(height, &network, subsidy)
            .unwrap()
            .remove(&FundingStreamReceiver::Deferred)
            .expect("the deferred funding stream is active")
    };

    let height = Height(12);
    let mut parent_pools = ValueBalance::<NonNegative>::zero();
    parent_pools.set_nsm_amount(Amount::try_from(1_000_000_000_i64)?);

    let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)?;
    let expected = deferred(height, subsidy);
    assert_ne!(
        expected,
        deferred(height, scheduled_block_subsidy(height, &network)?)
    );

    assert_eq!(
        calculate_deferred_pool_balance_change(height, &network, parent_pools)?.value(),
        expected.constrain::<zebra_chain::amount::NegativeAllowed>()?,
    );

    // Before activation the parent's chain value pools don't matter.
    let height = Height(9);
    assert_eq!(
        calculate_deferred_pool_balance_change(height, &network, parent_pools)?,
        calculate_deferred_pool_balance_change(height, &network, ValueBalance::zero())?,
    );

    Ok(())
}

/// Committed blocks track the NSM value balance: the NU7 activation block seeds it, blocks before
/// the NSM reissuance height leave it unchanged, and from the deployment height each block
/// reissues from the balance after its parent. A block paying the scheduled subsidy instead is
/// rejected from the deployment height.
///
/// Each block after activation credits the balance with the fees it removes from circulation.
/// From the deployment height a block whose coinbase claims those fees is rejected.
#[test]
fn nsm_subsidy_tracks_the_nsm_value_balance() -> Result<()> {
    use chrono::Duration;
    use zebra_chain::{
        block::{
            ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash,
            CHAIN_HISTORY_ACTIVATION_RESERVED,
        },
        parameters::{
            subsidy::{
                additional_block_subsidy, block_subsidy_with_parent_pools, nsm_fee_contribution,
                scheduled_block_subsidy, CoinbaseTransactionError, SubsidyError,
            },
            testnet::{ConfiguredActivationHeights, RegtestParameters},
        },
        transaction::{self, LockTime, Transaction},
    };

    use crate::ValidateContextError;

    let _init_guard = zebra_test::init();

    // Every upgrade from Heartwood activates together, so the activation block's parent is
    // pre-Heartwood and its block commitment only needs the reserved chain history root.
    const ACTIVATION_HEIGHT: u32 = 11;
    const DEPLOYMENT_HEIGHT: u32 = ACTIVATION_HEIGHT + 2;
    const INITIAL_NSM_VALUE_BALANCE: i64 = 1_000_000_000;
    // Each block after activation spends two outputs of `FUNDING_VALUE`, paying these fees. ZIP 235
    // rounds the fees it removes once per block: `floor(1_002 * 6 / 10)` is 601, but rounding each
    // transaction's fee would remove 600.
    const FUNDING_VALUE: i64 = 10_000;
    const FEES: [i64; 2] = [1_001, 1];

    let zats = |zats: i64| Amount::<NonNegative>::try_from(zats).expect("valid amount");

    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            sapling: Some(1),
            blossom: Some(1),
            heartwood: Some(ACTIVATION_HEIGHT),
            canopy: Some(ACTIVATION_HEIGHT),
            nu5: Some(ACTIVATION_HEIGHT),
            nu6: Some(ACTIVATION_HEIGHT),
            nu6_3: Some(ACTIVATION_HEIGHT),
            nu7: Some(ACTIVATION_HEIGHT),
            ..Default::default()
        },
        funding_streams: Some(Vec::new()),
        initial_nsm_value_balance: Some(zats(INITIAL_NSM_VALUE_BALANCE)),
        nsm_reissuance_height: Some(Height(DEPLOYMENT_HEIGHT)),
        ..Default::default()
    });

    // The outputs that the transactions in the block at `height` spend. They aren't created by any
    // transaction, so the test adds them to the outputs of the block that spends them.
    let funding_outpoints = |height: Height| {
        (0..FEES.len() as u32).map(move |index| transparent::OutPoint {
            hash: transaction::Hash([height.0 as u8; 32]),
            index,
        })
    };
    let funding_output = || transparent::Output {
        value: zats(FUNDING_VALUE),
        lock_script: transparent::Script::new(&[]),
    };
    let prepare_with_funding = |block: Arc<Block>, height: Height| {
        let mut prepared = block.prepare();
        Arc::make_mut(&mut prepared.new_outputs).extend(funding_outpoints(height).map(
            |outpoint| {
                let utxo = transparent::Utxo::new(funding_output(), height, false);
                (outpoint, transparent::OrderedUtxo::from_utxo(utxo, 0))
            },
        ));
        prepared
    };

    // The transactions in the block at `height`, which spend that block's funding outputs and pay
    // `FEES`.
    let spends = |height: Height| -> Vec<Arc<Transaction>> {
        funding_outpoints(height)
            .zip(FEES)
            .map(|(outpoint, fee)| {
                Arc::new(Transaction::test_v4(
                    vec![transparent::Input::PrevOut {
                        outpoint,
                        unlock_script: transparent::Script::new(&[]),
                        sequence: u32::MAX,
                    }],
                    vec![transparent::Output {
                        value: zats(FUNDING_VALUE - fee),
                        lock_script: transparent::Script::new(&[]),
                    }],
                    LockTime::unlocked(),
                    height,
                ))
            })
            .collect()
    };
    let fees = zats(FEES.iter().sum());

    // A block at `height` whose coinbase pays `coinbase_value`, followed by `transactions`. Block
    // times increase with height, so each block's time is after the median time of the blocks
    // before it.
    let block = |height: Height,
                 previous_block_hash,
                 coinbase_value,
                 transactions: Vec<Arc<Transaction>>|
     -> Arc<Block> {
        let mut block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let header = Arc::make_mut(&mut block.header);
        header.time += Duration::minutes(height.0.into());

        block.transactions = vec![Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output {
                value: coinbase_value,
                lock_script: transparent::Script::new(&[]),
            }],
            LockTime::unlocked(),
            height,
        ))];
        block.transactions.extend(transactions);
        Arc::make_mut(&mut block.header).previous_block_hash = previous_block_hash;

        Arc::new(block)
    };

    // A block at `height` whose coinbase pays `coinbase_value`, followed by `transactions`,
    // committing to `history_root`.
    let committed_block = |height: Height,
                           previous_block_hash,
                           coinbase_value,
                           transactions,
                           history_root: &ChainHistoryMmrRootHash| {
        let child = block(height, previous_block_hash, coinbase_value, transactions);
        let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            history_root,
            &child.auth_data_root(),
        );

        child.set_block_commitment(commitment.into())
    };

    // A block at `height` paying the ZIP's subsidy for `parent_pools`, and the part of `fees` that
    // ZIP 235 leaves to the miner, committing to `history_root`.
    let subsidy_block = |height: Height,
                         previous_block_hash,
                         parent_pools,
                         fees: Amount<NonNegative>,
                         transactions,
                         history_root: &_| {
        let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)
            .expect("the block subsidy is valid");
        let miner_fees = (fees - nsm_fee_contribution(height, &network, fees))
            .expect("the contribution is at most the fees");

        committed_block(
            height,
            previous_block_hash,
            (subsidy + miner_fees).expect("valid amount"),
            transactions,
            history_root,
        )
    };

    let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)?;

    let parent_height = Height(ACTIVATION_HEIGHT - 1);
    let activation = Height(ACTIVATION_HEIGHT);

    // The block before activation pays the scheduled subsidy, and leaves the NSM value balance empty.
    let parent = block(
        parent_height,
        block::Hash([1; 32]),
        scheduled_block_subsidy(parent_height, &network)?,
        Vec::new(),
    );

    let mut state = NonFinalizedState::new(&network);
    state.commit_new_chain(parent.clone().prepare(), &finalized_state)?;
    let pools_before_activation = state
        .best_chain()
        .expect("the chain was just committed")
        .chain_value_pools;
    assert_eq!(pools_before_activation.nsm_amount(), zats(0));

    // The activation block seeds the balance, and pays only the scheduled subsidy because the
    // deployment height is later.
    let initial = zats(INITIAL_NSM_VALUE_BALANCE);
    let activation_block = subsidy_block(
        activation,
        parent.hash(),
        pools_before_activation,
        zats(0),
        Vec::new(),
        &CHAIN_HISTORY_ACTIVATION_RESERVED.into(),
    );
    assert_eq!(
        activation_block.transactions[0].outputs()[0].value(),
        scheduled_block_subsidy(activation, &network)?,
    );
    state.commit_block(activation_block.clone().prepare(), &finalized_state)?;

    let chain = state
        .best_chain()
        .expect("the activation block was committed");
    assert_eq!(chain.chain_value_pools.nsm_amount(), initial);

    // Later blocks commit to the chain history tree, which starts at the activation block. The
    // blocks have no shielded transactions, so the note commitment tree roots never change.
    let sapling_root = chain.sapling_note_commitment_tree_for_tip().root();
    let orchard_root = chain.orchard_note_commitment_tree_for_tip().root();
    let ironwood_root = chain.ironwood_note_commitment_tree_for_tip().root();
    let roots = || BlockCommitmentTreeRoots {
        sapling: &sapling_root,
        orchard: &orchard_root,
        ironwood: &ironwood_root,
    };
    let mut tree = NonEmptyHistoryTree::from_block(&network, activation_block, roots())?;

    let mut previous = state.best_tip().expect("the chain has a tip");
    for height in (ACTIVATION_HEIGHT + 1..=DEPLOYMENT_HEIGHT + 2).map(Height) {
        let parent_pools = state
            .best_chain()
            .expect("the chain has blocks")
            .chain_value_pools;
        let reissued = additional_block_subsidy(height, &network, parent_pools.nsm_amount());
        let contributed = nsm_fee_contribution(height, &network, fees);
        let miner_fees = (fees - contributed)?;
        assert_eq!(contributed, zats(601));

        if height < Height(DEPLOYMENT_HEIGHT) {
            assert_eq!(
                reissued,
                zats(0),
                "no reissuance before the deployment height"
            );
        } else {
            assert!(reissued > zats(0), "reissuance from the deployment height");

            // From the deployment height, a block paying only the scheduled subsidy is rejected,
            // and so is one claiming the fees that ZIP 235 removes from circulation.
            let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)?;
            let mut invalid_coinbase_values =
                vec![(scheduled_block_subsidy(height, &network)? + miner_fees)?];
            if !contributed.is_zero() {
                invalid_coinbase_values.push((subsidy + fees)?);
            }

            for coinbase_value in invalid_coinbase_values {
                let invalid = committed_block(
                    height,
                    previous.1,
                    coinbase_value,
                    spends(height),
                    &tree.hash(),
                );
                assert!(matches!(
                    state.commit_block(prepare_with_funding(invalid, height), &finalized_state),
                    Err(ValidateContextError::InvalidSubsidy {
                        subsidy_error: CoinbaseTransactionError::Subsidy(
                            SubsidyError::InvalidMinerFees
                        ),
                        ..
                    })
                ));
            }
        }

        let next_block = subsidy_block(
            height,
            previous.1,
            parent_pools,
            fees,
            spends(height),
            &tree.hash(),
        );
        state.commit_block(
            prepare_with_funding(next_block.clone(), height),
            &finalized_state,
        )?;

        // The block debits exactly what it reissued, and credits the fees it removed from
        // circulation.
        let chain = state.best_chain().expect("the block was committed");
        assert_eq!(
            chain.chain_value_pools.nsm_amount(),
            ((parent_pools.nsm_amount() - reissued)? + contributed)?,
        );

        tree.push(next_block, roots())?;
        previous = state.best_tip().expect("the chain has a tip");
    }

    Ok(())
}

/// Build equal-work siblings with explicit first- and second-received stamps.
fn precious_test_forks() -> (NonFinalizedState, FinalizedState, [Arc<Block>; 3]) {
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let first = root
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([1; 32]);
    let second = root
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([2; 32]);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .unwrap();
    let received = std::time::Instant::now();
    for (block, delay) in [(&first, 0), (&second, 1)] {
        let mut prepared = block.clone().prepare();
        prepared.received_time = Some(received + Duration::from_secs(delay));
        state.commit_block(prepared, &finalized_state).unwrap();
    }
    assert_eq!(state.best_tip().unwrap().1, first.hash());
    (state, finalized_state, [root, first, second])
}

#[test]
fn precious_block_prefers_second_received_tip() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [_, first, second]) = precious_test_forks();
    let snapshot = state.clone();
    assert_eq!(
        state
            .precious_block(second.hash(), &finalized_state.db)
            .unwrap(),
        second.hash()
    );
    assert_eq!(state.best_tip().unwrap().1, second.hash());
    assert_eq!(state.chain_count(), 2);
    assert_eq!(snapshot.best_tip().unwrap().1, first.hash());
}

#[test]
fn precious_block_latest_call_wins() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [_, first, second]) = precious_test_forks();
    for tip in [&second, &first, &second] {
        state
            .precious_block(tip.hash(), &finalized_state.db)
            .unwrap();
        assert_eq!(state.best_tip().unwrap().1, tip.hash());
        assert_eq!(state.chain_count(), 2);
    }
    let before = state.clone();
    state
        .precious_block(second.hash(), &finalized_state.db)
        .unwrap();
    assert!(
        state.eq_internal_state(&before),
        "repeating the latest call must not change the state"
    );
}

#[test]
fn precious_block_never_overrides_more_work() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [_, first, second]) = precious_test_forks();
    state
        .precious_block(second.hash(), &finalized_state.db)
        .unwrap();
    assert_eq!(state.best_tip().unwrap().1, second.hash());
    let child = first.make_fake_child().set_work(10);
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    assert_eq!(state.best_tip().unwrap().1, child.hash());
}

#[test]
fn precious_block_lower_work_call_leaves_no_preference() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [_, first, second]) = precious_test_forks();
    let child = first.make_fake_child().set_work(10);
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    let before = state.clone();
    state
        .precious_block(second.hash(), &finalized_state.db)
        .unwrap();
    assert_eq!(state.best_tip().unwrap().1, child.hash());
    assert!(
        state.eq_internal_state(&before),
        "a lower-work call must not store a rank"
    );
    assert!(state
        .chain_iter()
        .all(|chain| chain.precious_rank.is_none()));
    state.invalidate_block(child.hash()).unwrap();
    assert_eq!(state.best_tip().unwrap().1, first.hash());
}

#[test]
fn precious_block_preference_drops_on_push_and_fork() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [root, first, second]) = precious_test_forks();
    state
        .precious_block(second.hash(), &finalized_state.db)
        .unwrap();
    let precious = state.best_chain().unwrap();
    assert!(precious.precious_rank.is_some());
    for hash in [root.hash(), second.hash()] {
        assert!(precious.fork(hash).unwrap().precious_rank.is_none());
    }
    let first_child = first.make_fake_child().set_work(10);
    let second_child = second.make_fake_child().set_work(10);
    let received = std::time::Instant::now();
    for (block, delay) in [(&first_child, 0), (&second_child, 1)] {
        let mut prepared = block.clone().prepare();
        prepared.received_time = Some(received + Duration::from_secs(delay));
        state.commit_block(prepared, &finalized_state).unwrap();
    }
    assert!(state
        .chain_iter()
        .all(|chain| chain.precious_rank.is_none()));
    assert_eq!(state.best_tip().unwrap().1, first_child.hash());
}

#[test]
fn precious_block_rejects_unknown_hash() {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, _) = precious_test_forks();
    let unknown = block::Hash([0; 32]);
    let before = state.clone();
    let error = state
        .precious_block(unknown, &finalized_state.db)
        .unwrap_err();
    assert_eq!(error.to_string(), "Block not found");
    assert!(matches!(error, PreciousError::MissingBlock(hash) if hash == unknown));
    assert!(state.eq_internal_state(&before));
}

#[test]
fn precious_block_accepts_known_non_tips_unchanged() {
    let _init_guard = zebra_test::init();
    let (mut state, mut finalized_state, [root, _, second]) = precious_test_forks();
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    finalized_state
        .commit_finalized_direct(
            crate::CheckpointVerifiedBlock::from(genesis.clone()).into(),
            None,
            "precious finalized no-op test",
        )
        .unwrap();
    state
        .precious_block(second.hash(), &finalized_state.db)
        .unwrap();
    assert_eq!(state.best_tip().unwrap().1, second.hash());
    let before = state.clone();
    for hash in [root.hash(), genesis.hash()] {
        assert_eq!(
            state.precious_block(hash, &finalized_state.db).unwrap(),
            hash
        );
        assert!(state.eq_internal_state(&before));
    }
}

#[test]
fn precious_block_preference_is_not_backed_up() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (mut state, finalized_state, [_, first, second]) = precious_test_forks();
    let backup_dir = tempfile::tempdir()?;
    state.write_to_backup(backup_dir.path());
    let baseline = super::super::backup::restore_backup(
        NonFinalizedState::new(&Network::Mainnet),
        backup_dir.path(),
        &finalized_state.db,
    );
    let preferred = if baseline.best_tip().unwrap().1 == first.hash() {
        second
    } else {
        first
    };
    state
        .precious_block(preferred.hash(), &finalized_state.db)
        .unwrap();
    assert_eq!(state.best_tip().unwrap().1, preferred.hash());
    assert!(state.best_chain().unwrap().precious_rank.is_some());
    state.write_to_backup(backup_dir.path());
    let restored = super::super::backup::restore_backup(
        NonFinalizedState::new(&Network::Mainnet),
        backup_dir.path(),
        &finalized_state.db,
    );
    assert_eq!(restored.chain_count(), 2);
    assert_eq!(restored.precious_rank, 0);
    assert!(restored
        .chain_iter()
        .all(|chain| chain.precious_rank.is_none()));
    assert_eq!(restored.best_tip(), baseline.best_tip());
    Ok(())
}

/// Regression test for the equal-work tie-break in `Chain::cmp`.
///
/// Sibling blocks always have equal work on Zcash (`nBits` is fully determined by their
/// ancestors), and the spec breaks equal-work ties by preferring the block received
/// first. Commits two equal-work siblings with controlled receipt times in both receipt
/// orders and checks that the first-received sibling stays best both times; one of the
/// two orders fails under hash tie-breaking by construction. Then extends the losing
/// sibling and checks that strictly more cumulative work still overrides receipt order.
#[test]
fn equal_work_ties_prefer_first_received() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    // Same parent and work, different commitment bytes: the siblings tie on cumulative
    // work with distinct hashes.
    let sibling_a = block1
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([0x01; 32]);
    let sibling_b = block1
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([0x02; 32]);
    assert_ne!(
        sibling_a.hash(),
        sibling_b.hash(),
        "siblings must have distinct hashes"
    );

    for (first, second) in [(&sibling_a, &sibling_b), (&sibling_b, &sibling_a)] {
        let (mut state, finalized_state) = new_invalidate_test_state(&network);

        state
            .commit_new_chain(block1.clone().prepare(), &finalized_state)
            .expect("fake root block should commit to an empty non-finalized state");

        // Control the verifier receipt stamp so construction order can't leak in:
        // `first` was received before `second`, regardless of commit order below.
        let earlier = std::time::Instant::now();
        let later = earlier + Duration::from_secs(1);

        let mut first_prepared = (*first).clone().prepare();
        first_prepared.received_time = Some(earlier);
        let mut second_prepared = (*second).clone().prepare();
        second_prepared.received_time = Some(later);

        state
            .commit_block(first_prepared, &finalized_state)
            .expect("first sibling should commit");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            first.hash(),
            "the first-received sibling is adopted as the best tip"
        );

        state
            .commit_block(second_prepared, &finalized_state)
            .expect("second sibling should commit");
        assert_eq!(2, state.chain_set.len(), "both sibling chains are tracked");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            first.hash(),
            "a later equal-work sibling must not displace the adopted tip"
        );

        // Strictly more cumulative work still overrides receipt order.
        let second_child = second.make_fake_child().set_work(10);
        state
            .commit_block(second_child.clone().prepare(), &finalized_state)
            .expect("child of the second sibling should commit");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            second_child.hash(),
            "more cumulative work must override receipt order"
        );
    }

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// At the fork limit, a newly inserted chain is kept even if it has the lowest work, and
/// equal-work ties evict the earliest-received chain other than the best chain. So a late
/// sibling survives both its own insertion and the insertion of another sibling after it.
#[test]
fn fork_limit_keeps_new_chains_and_evicts_earliest_received_ties() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;

    let received = std::time::Instant::now();
    let mut commit = |block: &Arc<Block>, secs: u64| {
        let mut prepared = block.clone().prepare();
        prepared.received_time = Some(received + Duration::from_secs(secs));
        state.commit_block(prepared, &finalized_state)
    };
    let sibling = |i: u8| {
        block1
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32])
    };

    let max_forks = u8::try_from(crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .expect("the fork limit fits in a u8");
    let siblings: Vec<_> = (0..=max_forks + 1).map(sibling).collect();
    for (secs, block) in (0..).zip(&siblings) {
        commit(block, secs)?;
    }
    let late = &siblings[usize::from(max_forks)];

    // A chain with strictly less work than all the others.
    let low = block1.make_fake_child().set_work(1);
    commit(&low, u64::from(max_forks) + 2)?;

    let late_child = late.make_fake_child().set_work(10);
    commit(&late_child, u64::from(max_forks) + 3)?;

    assert_eq!(
        crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        state.chain_count()
    );
    for (i, block) in siblings.iter().enumerate() {
        assert_eq!(
            !(1..=3).contains(&i),
            state.any_chain_contains(&block.hash()),
            "only the earliest-received siblings after the best one are evicted, sibling {i}",
        );
    }
    assert!(state.any_chain_contains(&low.hash()));
    assert_eq!(
        state.best_chain().unwrap().non_finalized_tip_hash(),
        late_child.hash(),
    );

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// After a restart that restores 10 equal-work forks from the non-finalized backup, a live
/// sibling is kept at the fork limit, and its child becomes the best chain tip. Restored
/// blocks have no receipt time, so they rank as received first.
#[test]
fn fork_limit_keeps_live_sibling_after_backup_restore() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    let sibling = |i: u8| {
        block1
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32])
    };

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    let max_forks = u8::try_from(crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .expect("the fork limit fits in a u8");
    for i in 0..max_forks {
        state.commit_block(sibling(i).prepare(), &finalized_state)?;
    }

    let backup_dir = tempfile::tempdir()?;
    state.write_to_backup(backup_dir.path());
    let mut state = super::super::backup::restore_backup(
        NonFinalizedState::new(&network),
        backup_dir.path(),
        &finalized_state.db,
    );
    assert_eq!(
        crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        state.chain_count()
    );
    assert!(state
        .chain_iter()
        .all(|chain| chain.tip_block().unwrap().received_time.is_none()));

    let live = sibling(max_forks);
    let mut prepared = live.clone().prepare();
    prepared.received_time = Some(std::time::Instant::now());
    state.commit_block(prepared, &finalized_state)?;
    assert!(state.any_chain_contains(&live.hash()));

    let live_child = live.make_fake_child().set_work(10);
    state.commit_block(live_child.clone().prepare(), &finalized_state)?;
    assert_eq!(
        state.best_chain().unwrap().non_finalized_tip_hash(),
        live_child.hash(),
    );

    Ok(())
}

/// Blocks evicted while restoring the backup were never sent to the block write task, so restoring
/// must not leave eviction notices for them. A stale notice would remove a later re-delivery of
/// the evicted block from the state service's sent hashes, so its children would never be
/// dispatched.
#[tokio::test]
async fn backup_restore_discards_eviction_notices() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    let siblings: Vec<_> = (0..=crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .map(|i| {
            block1
                .make_fake_child()
                .set_work(10)
                .set_block_commitment([u8::try_from(i).expect("the fork limit fits in a u8"); 32])
        })
        .collect();

    // Back up one more fork than the limit allows, by merging the backups of two states.
    let (last_sibling, kept_siblings) = siblings.split_last().expect("siblings are not empty");
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for sibling in kept_siblings {
        state.commit_block(sibling.clone().prepare(), &finalized_state)?;
    }
    let backup_dir = tempfile::tempdir()?;
    state.write_to_backup(backup_dir.path());

    let mut last_state = NonFinalizedState::new(&network);
    last_state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    last_state.commit_block(last_sibling.clone().prepare(), &finalized_state)?;
    let last_backup_dir = tempfile::tempdir()?;
    last_state.write_to_backup(last_backup_dir.path());
    for entry in std::fs::read_dir(last_backup_dir.path())? {
        let entry = entry?;
        std::fs::copy(entry.path(), backup_dir.path().join(entry.file_name()))?;
    }

    let (mut restored, _sender, _receiver) = NonFinalizedState::new(&network)
        .with_backup(
            Some(backup_dir.path().to_path_buf()),
            &finalized_state.db,
            true,
            true,
        )
        .await;
    assert_eq!(
        crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        restored.chain_count()
    );
    let restore_evicted = siblings
        .iter()
        .find(|sibling| !restored.any_chain_contains(&sibling.hash()))
        .expect("restoring evicts one fork");

    let mut sent_hashes = crate::service::queued_blocks::SentHashes::new(&restored);
    assert!(!sent_hashes.contains(&restore_evicted.hash()));
    assert!(
        restored.take_evicted_block_hashes().is_empty(),
        "restoring leaves no eviction notices"
    );

    // Re-delivering the evicted block keeps it sent, so the state service dispatches its child.
    let mut redelivered = restore_evicted.clone().prepare();
    redelivered.received_time = Some(std::time::Instant::now());
    sent_hashes.add(&redelivered);
    restored.commit_block(redelivered, &finalized_state)?;
    sent_hashes.remove_all(restored.take_evicted_block_hashes());
    assert!(sent_hashes.can_fork_chain_at(&restore_evicted.hash()));

    let child = restore_evicted.make_fake_child().set_work(10);
    restored.commit_block(child.clone().prepare(), &finalized_state)?;
    assert_eq!(
        restored.best_chain().unwrap().non_finalized_tip_hash(),
        child.hash(),
    );

    Ok(())
}

/// Restoring a backup that is over the index entry limit, such as one written before Mainnet had
/// the limit, stops at the first block that the block write task would finalize, instead of
/// rebuilding every backed up block in memory.
#[test]
fn backup_restore_stops_at_the_index_entry_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let min_blocks = crate::constants::MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.max_index_entries = None;
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    let mut tip = block1;
    while state.best_chain_len() < Some(min_blocks + 20) {
        tip = tip.make_fake_child();
        state.commit_block(tip.clone().prepare(), &finalized_state)?;
    }

    let backup_dir = tempfile::tempdir()?;
    state.write_to_backup(backup_dir.path());

    let mut limited = NonFinalizedState::new(&network);
    limited.max_index_entries = Some(1);
    let restored =
        super::super::backup::restore_backup(limited, backup_dir.path(), &finalized_state.db);
    assert_eq!(Some(min_blocks + 1), restored.best_chain_len());
    assert!(restored.should_finalize_root());

    // A backup within the limits is restored whole.
    let restored = super::super::backup::restore_backup(
        NonFinalizedState::new(&network),
        backup_dir.path(),
        &finalized_state.db,
    );
    assert_eq!(Some(min_blocks + 20), restored.best_chain_len());

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// Test networks keep more equal-work sibling forks than Mainnet before evicting any.
#[test]
fn fork_limit_is_higher_on_test_networks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let siblings = 30u8;
    for (network, expected_forks) in [
        (
            Network::Mainnet,
            crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        ),
        (Network::new_default_testnet(), usize::from(siblings)),
    ] {
        let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let (mut state, finalized_state) = new_invalidate_test_state(&network);
        state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;

        for i in 0..siblings {
            let sibling = block1
                .make_fake_child()
                .set_work(10)
                .set_block_commitment([i; 32]);
            state.commit_block(sibling.prepare(), &finalized_state)?;
        }

        assert_eq!(expected_forks, state.chain_count(), "{network}");
    }

    Ok(())
}

/// The backup task writes each block once, even when several chains share it.
#[test]
fn backup_update_skips_blocks_shared_by_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for i in 0..3u8 {
        let sibling = block1.make_fake_child().set_block_commitment([i; 32]);
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }
    assert_eq!(3, state.chain_count());

    // Claim every block is already backed up, so nothing should be written.
    let backup_dir = tempfile::tempdir()?;
    let backup_blocks = state
        .chain_iter()
        .flat_map(|chain| chain.blocks.values())
        .map(|block| (block.hash, backup_dir.path().join("unused")))
        .collect();
    super::super::backup::update_non_finalized_state_backup(
        backup_dir.path(),
        &state,
        backup_blocks,
    );

    assert_eq!(0, std::fs::read_dir(backup_dir.path())?.count());

    Ok(())
}

/// Returns a fake child of `parent` whose coinbase has the outputs of `template`'s coinbase,
/// followed by `count` zero-value outputs paying `addresses` distinct addresses.
fn busy_child(
    parent: &Arc<Block>,
    template: &Block,
    network: &Network,
    count: u32,
    addresses: u32,
) -> Arc<Block> {
    let mut block = Block::clone(&parent.make_fake_child());
    let coinbase = Arc::unwrap_or_clone(block.transactions.remove(0));
    let mut outputs = template.transactions[0].outputs().to_vec();
    outputs.extend((0..count).map(|i| {
        let mut pub_key_hash = [0; 20];
        pub_key_hash[..4].copy_from_slice(&(i % addresses.max(1)).to_le_bytes());
        transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Address::from_pub_key_hash(network.kind(), pub_key_hash)
                .script(),
        }
    }));
    block
        .transactions
        .insert(0, Arc::new(coinbase.with_transparent_outputs(outputs)));
    Arc::new(block)
}

/// Finalizing a busy root block with several forks, while a published snapshot shares their
/// indexes, gives the same chains as popping each chain's root separately, and the forks keep
/// sharing the busy block's records with the best chain instead of each copying them.
#[test]
fn finalizing_keeps_block_records_shared_between_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, 5_000, 50);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(busy.clone().prepare(), &finalized_state)?;
    for i in 0..5u8 {
        let sibling = busy_child(&busy, &block1, &network, 0, 1)
            .set_work(10)
            .set_block_commitment([i; 32]);
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }
    assert_eq!(5, state.chain_count());

    // Each chain with its root block popped separately.
    let mut expected: Vec<Chain> = state
        .chain_iter()
        .map(|chain| {
            let mut chain = Chain::clone(chain);
            chain.pop_root();
            chain
        })
        .collect();

    // The write task publishes a snapshot that shares every chain before it finalizes.
    let published = state.clone();
    let finalized = state.finalize();
    assert_eq!(finalized.inner_block().hash(), block1.hash());

    let chains: Vec<_> = state.chain_iter().cloned().collect();
    assert_eq!(expected.len(), chains.len());
    for (chain, expected) in chains.iter().zip(expected.drain(..)) {
        assert!(chain.eq_internal_state(&expected));
        crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes(chain);
    }

    // Every chain, and the published snapshot, still shares the busy block and its address
    // changes, instead of each holding its own copy.
    let busy_height = busy.coinbase_height().unwrap();
    let best = &chains[0];
    for chain in chains[1..].iter().chain(published.chain_iter()) {
        assert!(Arc::ptr_eq(
            &chain.blocks[&busy_height],
            &best.blocks[&busy_height]
        ));
        assert!(Arc::ptr_eq(
            &chain.transparent_transfers_by_height[&busy_height],
            &best.transparent_transfers_by_height[&busy_height],
        ));
    }
    assert!(best.transparent_transfers_by_height[&busy_height].len() >= 50);

    Ok(())
}

/// Adds `count` transactions to `block`, each with one output that has no value.
///
/// Blocks in the same chain need different `seed`s, so their transactions have different hashes.
fn with_transactions(block: &Block, network: &Network, count: u32, seed: u8) -> Arc<Block> {
    let mut block = block.clone();
    let template = zebra_chain::transaction::Transaction::clone(&block.transactions[0])
        .with_transparent_inputs(Vec::new());
    for i in 0..count {
        let mut pub_key_hash = [0xaa; 20];
        pub_key_hash[..4].copy_from_slice(&i.to_le_bytes());
        pub_key_hash[4] = seed;
        let output = transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Address::from_pub_key_hash(network.kind(), pub_key_hash)
                .script(),
        };
        block.transactions.push(Arc::new(
            template.clone().with_transparent_outputs(vec![output]),
        ));
    }
    Arc::new(block)
}

/// Returns a child of `parent` with a transaction that spends `outpoints`.
fn spending_child(parent: &Arc<Block>, outpoints: &[transparent::OutPoint]) -> Arc<Block> {
    let mut block = Block::clone(&parent.make_fake_child());
    let spend = zebra_chain::transaction::Transaction::clone(&block.transactions[0])
        .with_transparent_inputs(
            outpoints
                .iter()
                .map(|outpoint| transparent::Input::PrevOut {
                    outpoint: *outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: 0,
                })
                .collect(),
        )
        .with_transparent_outputs(Vec::new());
    block.transactions.push(Arc::new(spend));
    Arc::new(block)
}

/// Finalizing blocks with spends keeps side chains sharing the spend index with the best chain,
/// instead of each chain removing the spends from its own copy.
#[test]
fn finalizing_keeps_side_chain_indexes_shared() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let funding = with_transactions(&block1.make_fake_child(), &network, 50, 0);
    let outpoints: Vec<_> = funding.transactions[1..]
        .iter()
        .map(|transaction| transparent::OutPoint {
            hash: transaction.hash(),
            index: 0,
        })
        .collect();
    let first_spends = spending_child(&funding, &outpoints[..25]);
    let second_spends = spending_child(&first_spends, &outpoints[25..]);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for block in [&funding, &first_spends, &second_spends] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    for i in 0..5u8 {
        let sibling = busy_child(&second_spends, &block1, &network, 0, 1)
            .set_work(10)
            .set_block_commitment([i; 32]);
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }
    assert_eq!(5, state.chain_count());

    for finalized in [&block1, &funding, &first_spends] {
        assert_eq!(state.finalize().inner_block().hash(), finalized.hash());

        let best = state.best_chain().unwrap().clone();
        assert!(!best.spent_utxos.is_empty());
        for chain in state.chain_iter() {
            assert!(chain.spent_utxos.ptr_eq(&best.spent_utxos));
            crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes(chain);
        }
    }
    assert_eq!(5, state.chain_count());

    Ok(())
}

/// Forks above a transaction-dense side-branch block each keep their own copy of its entries
/// after rebuilding their maps from the best chain, so the state counts every copy.
#[test]
fn rebuilt_forks_count_their_side_branch_entries() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy_root = with_transactions(&block1.make_fake_child(), &network, 2_000, 0);
    let common = busy_root.make_fake_child();
    let best = common.make_fake_child();
    let dense_side = with_transactions(&common.make_fake_child(), &network, 500, 1).set_work(10);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for block in [&busy_root, &common, &best, &dense_side] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    for i in 0..3u8 {
        let tip = dense_side
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32]);
        state.commit_block(tip.prepare(), &finalized_state)?;
    }
    assert_eq!(4, state.chain_count());

    // Popping the busy root separately is enough to rebuild every side chain.
    state.finalize();
    state.finalize();
    assert_eq!(4, state.chain_count());

    let best_tip = state.best_tip().unwrap().1;
    let mut side_entries = 0;
    for chain in state.chain_iter() {
        if chain.non_finalized_tip_hash() == best_tip {
            continue;
        }
        assert_eq!(chain.index_removals_since_rebuild, 0);
        assert!(chain.detached_index_entry_count() > 500);
        side_entries += chain.detached_index_entry_count();
    }
    assert!(state.index_entry_count() > side_entries + 500);

    Ok(())
}

/// A side chain that rebuilt its maps from a transaction-dense best chain waits for as many new
/// removals as the chains' divergent entries before rebuilding again.
#[test]
fn small_side_chain_rebuilds_after_enough_removals() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy_root = with_transactions(&block1.make_fake_child(), &network, 2_000, 0);
    let common = busy_root.make_fake_child();
    let dense_best = with_transactions(&common.make_fake_child(), &network, 500, 1);
    let small_side = common.make_fake_child().set_work(10);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for block in [&busy_root, &common, &dense_best, &small_side] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    assert_eq!(2, state.chain_count());

    let side_chain = |state: &NonFinalizedState| {
        state
            .chain_iter()
            .find(|chain| chain.non_finalized_tip_hash() == small_side.hash())
            .unwrap()
            .clone()
    };

    state.finalize();
    state.finalize();
    assert_eq!(side_chain(&state).index_removals_since_rebuild, 0);

    // Removing the small common block doesn't pay for another rebuild.
    assert_eq!(state.finalize().inner_block().hash(), common.hash());
    let side = side_chain(&state);
    assert!(side.index_removals_since_rebuild > 0);
    // Removing the best chain's dense block didn't leave the small chain holding its entries.
    assert!(side.detached_index_entry_count() < 10);

    Ok(())
}

/// A fork that becomes the best chain stops counting the entries it detached when it forked, once
/// it finalizes every block it held then.
#[test]
fn detached_index_entries_retire_after_finalization() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child();
    let old3 = block2.make_fake_child();
    let old4 = old3.make_fake_child();
    let new3 = block2.make_fake_child().set_block_commitment([1; 32]);
    let new4 = new3.make_fake_child();
    let new5 = new4.make_fake_child();
    let new6 = new5.make_fake_child();

    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for block in [&block2, &old3, &old4, &new3, &new4, &new5, &new6] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    assert_eq!(state.best_tip().unwrap().1, new6.hash());
    assert!(state.best_chain().unwrap().detached_index_entry_count() > 0);

    for finalized in [&block1, &block2, &new3] {
        assert_eq!(state.finalize().inner_block().hash(), finalized.hash());
    }
    // The fork detached entries while it held `old4`'s height.
    assert!(state.best_chain().unwrap().detached_index_entry_count() > 0);

    assert_eq!(state.finalize().inner_block().hash(), new4.hash());
    assert_eq!(state.best_chain().unwrap().detached_index_entry_count(), 0);

    Ok(())
}

/// A side chain that rebuilt its maps from a fork keeps counting the entries that fork detached,
/// after a third fork wins and evicts it.
#[test]
fn rebuilt_chains_keep_their_source_detached_entries() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy_root = with_transactions(&block1.make_fake_child(), &network, 2_000, 0);
    let old3 = busy_root.make_fake_child();
    let dense_old4 = with_transactions(&old3.make_fake_child(), &network, 500, 1);
    let new3 = busy_root.make_fake_child().set_block_commitment([1; 32]);
    let new4 = new3.make_fake_child();
    let new5 = new4.make_fake_child();
    let new6 = new5.make_fake_child();
    let side4 = new3.make_fake_child().set_work(10);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for block in [
        &busy_root,
        &old3,
        &dense_old4,
        &new3,
        &new4,
        &new5,
        &new6,
        &side4,
    ] {
        state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    assert_eq!(state.best_tip().unwrap().1, new6.hash());
    // The best chain forked from the dense old chain.
    assert!(state.best_chain().unwrap().detached_index_entry_count() > 500);

    state.finalize();
    let side_chain = |state: &NonFinalizedState, tip: block::Hash| {
        state
            .chain_iter()
            .find(|chain| chain.non_finalized_tip_hash() == tip)
            .unwrap()
            .clone()
    };
    assert_eq!(
        side_chain(&state, side4.hash()).index_removals_since_rebuild,
        0
    );

    // A heavier block on the side chain makes it the best chain, and evicts every other fork.
    state.max_index_entries = Some(1);
    let winner = side4.make_fake_child().set_work(1 << 80);
    state.commit_block(winner.clone().prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_count());
    assert_eq!(state.best_tip().unwrap().1, winner.hash());

    let winner_chain = side_chain(&state, winner.hash());
    assert!(winner_chain.detached_index_entry_count() > 500);
    assert!(state.index_entry_count() > 2_000 + 500);

    Ok(())
}

/// Forks over a busy block share its transactions and created outputs, so the state counts them
/// once.
#[test]
fn index_entries_count_shared_blocks_once() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = with_transactions(
        &busy_child(&block1, &block1, &network, 5_000, 50),
        &network,
        1_000,
        0,
    );
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(busy.clone().prepare(), &finalized_state)?;
    let one_chain = state.index_entry_count();

    for i in 0..5u8 {
        let sibling = busy_child(&busy, &block1, &network, 0, 1)
            .set_work(10)
            .set_block_commitment([i; 32]);
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }

    let logical: usize = state
        .chain_iter()
        .map(|chain| chain.index_entry_count())
        .sum();
    assert!(logical > 5 * (5_000 + 1_000));
    // Each fork adds only its own small tip block.
    assert!(state.index_entry_count() < one_chain + 5 * 10);

    // Each distinct block counts itself, its outputs, transactions, spends, and nullifiers.
    let mut seen = std::collections::HashSet::new();
    let block_entries: usize = state
        .chain_iter()
        .flat_map(|chain| chain.blocks.values())
        .filter(|block| seen.insert(block.hash))
        .map(|block| {
            let transactions = &block.block.transactions;
            1 + block.new_outputs.len()
                + transactions.len()
                + transactions
                    .iter()
                    .map(|transaction| {
                        transaction.spent_outpoints().count()
                            + transaction.sprout_nullifiers().count()
                            + transaction.sapling_nullifiers().count()
                            + transaction.orchard_nullifiers().count()
                            + transaction.ironwood_nullifiers().count()
                    })
                    .sum::<usize>()
        })
        .sum();
    let detached: usize = state
        .chain_iter()
        .map(|chain| chain.detached_index_entry_count())
        .sum();
    assert!(block_entries > 6_000);
    assert_eq!(state.index_entry_count(), block_entries + detached);

    Ok(())
}

/// Returns the resident set size of this process in bytes, or zero if it is not available.
fn resident_set_size() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))?
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kib| kib * 1024)
}

/// Returns whether to run a memory measurement test.
///
/// CI also runs ignored tests, so the measurements need `ZEBRA_TEST_MEASURE_MEMORY` as well.
#[allow(clippy::print_stderr)]
fn memory_measurement_requested() -> bool {
    let requested = std::env::var_os("ZEBRA_TEST_MEASURE_MEMORY").is_some();
    if !requested {
        eprintln!("Skipping memory measurement because '$ZEBRA_TEST_MEASURE_MEMORY' is not set.");
    }
    requested
}

/// Measures non-finalized state memory when many equal-work forks span a busy block, as on
/// Testnet after height 4,602,598 (#12416).
///
/// Run with `ZEBRA_TEST_MEASURE_MEMORY=1` and `--run-ignored=only --no-capture`.
/// `ZEBRA_TEST_BUSY_OUTPUTS` sets the outputs in the busy block, and `ZEBRA_TEST_BUSY_FORKS` sets
/// the sibling forks committed on top of it.
#[test]
#[ignore]
#[allow(clippy::print_stdout)]
fn measure_fork_memory_over_busy_block() -> Result<()> {
    let _init_guard = zebra_test::init();
    if !memory_measurement_requested() {
        return Ok(());
    }

    let env = |name, default| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let outputs: u32 = env("ZEBRA_TEST_BUSY_OUTPUTS", 200_000);
    let forks: u32 = env("ZEBRA_TEST_BUSY_FORKS", 100);
    let mib = |bytes: u64| bytes / (1024 * 1024);

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, outputs, 1_000);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    // `ZEBRA_TEST_INDEX_LIMIT=none` measures without the index entry limit.
    if let Ok(limit) = std::env::var("ZEBRA_TEST_INDEX_LIMIT") {
        state.max_index_entries = limit.parse().ok();
    }

    let start = resident_set_size();
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(busy.clone().prepare(), &finalized_state)?;
    let one_chain = resident_set_size();

    for i in 0..forks {
        let mut sibling = busy_child(&busy, &block1, &network, 0, 1)
            .set_work(10)
            .set_block_commitment([0; 32]);
        // Distinct block commitments only fit 256 siblings, so also vary the timestamp.
        Arc::make_mut(&mut Arc::make_mut(&mut sibling).header).time +=
            chrono::Duration::seconds(i.into());
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }
    let all_forks = resident_set_size();

    // The write task publishes a clone of the state before it finalizes (#11820).
    let published = state.clone();
    state.finalize();
    let finalized = resident_set_size();
    drop(published);
    let after_publish = resident_set_size();

    let entries: Vec<usize> = state
        .chain_iter()
        .map(|chain| chain.index_entry_count())
        .collect();
    println!(
        "outputs {outputs}, forks {forks}, limit {:?}: chains {}, index entries {}, \
         RSS MiB start {}, one chain {}, all forks {}, after finalize {}, \
         after dropping the published state {}",
        state.max_index_entries,
        state.chain_count(),
        entries.iter().sum::<usize>(),
        mib(start),
        mib(one_chain),
        mib(all_forks),
        mib(finalized),
        mib(after_publish),
    );

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/12416.
///
/// On test networks, forks with their own busy blocks are evicted when the index entries of all
/// chains pass the limit, before the fork limit is reached. The best chain and the newest fork
/// are always kept, even when they alone pass the limit.
#[test]
fn index_entry_limit_evicts_forks_with_busy_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, 1_000, 10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    assert_eq!(
        Some(crate::constants::MAX_NON_FINALIZED_INDEX_ENTRIES_TEST_NETWORKS),
        state.max_index_entries,
    );

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(busy.clone().prepare(), &finalized_state)?;

    let received = std::time::Instant::now();
    let siblings: Vec<_> = (0..10u8)
        .map(|i| {
            busy_child(&busy, &block1, &network, 1_000, 10)
                .set_work(10)
                .set_block_commitment([i; 32])
        })
        .collect();
    let commit = |state: &mut NonFinalizedState, i: usize| {
        let mut prepared = siblings[i].clone().prepare();
        prepared.received_time = Some(received + Duration::from_secs(i.try_into().unwrap()));
        state.commit_block(prepared, &finalized_state)
    };

    // Each fork adds the outputs of its own busy block.
    commit(&mut state, 0)?;
    commit(&mut state, 1)?;
    let max_index_entries = 2 * state.index_entry_count();
    state.max_index_entries = Some(max_index_entries);
    for i in 2..siblings.len() {
        commit(&mut state, i)?;
    }

    assert!(state.chain_count() < siblings.len());
    assert!(state.chain_count() == 2 || state.index_entry_count() <= max_index_entries);
    assert!(
        state.any_chain_contains(&siblings[0].hash()),
        "the best chain is kept"
    );
    assert!(
        state.any_chain_contains(&siblings[9].hash()),
        "the newest fork is kept"
    );
    for chain in state.chain_iter() {
        crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes(chain);
    }

    // Only the evicted siblings are reported, not the blocks they share with the kept chains.
    let evicted: std::collections::HashSet<_> = siblings
        .iter()
        .map(|sibling| sibling.hash())
        .filter(|hash| !state.any_chain_contains(hash))
        .collect();
    let reported = state.take_evicted_block_hashes();
    assert_eq!(evicted.len(), reported.len());
    assert_eq!(evicted, reported.into_iter().collect());
    assert!(state.take_evicted_block_hashes().is_empty());

    // A limit below a single chain keeps the best chain and the newest fork.
    state.max_index_entries = Some(1);
    let late = busy_child(&busy, &block1, &network, 1_000, 10)
        .set_work(10)
        .set_block_commitment([10; 32]);
    let mut prepared = late.clone().prepare();
    prepared.received_time = Some(received + Duration::from_secs(10));
    state.commit_block(prepared, &finalized_state)?;
    assert_eq!(2, state.chain_count());
    assert!(state.any_chain_contains(&siblings[0].hash()));
    assert!(state.any_chain_contains(&late.hash()));

    let late_child = late.make_fake_child().set_work(10);
    state.commit_block(late_child.clone().prepare(), &finalized_state)?;
    assert_eq!(
        state.best_chain().unwrap().non_finalized_tip_hash(),
        late_child.hash(),
    );

    Ok(())
}

/// Mainnet limits index entries too, to the size of the blocks it keeps when finalizing early.
#[test]
fn index_entry_limit_is_higher_on_mainnet() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        Some(crate::constants::MAX_NON_FINALIZED_INDEX_ENTRIES),
        NonFinalizedState::new(&Network::Mainnet).max_index_entries
    );
    assert_eq!(
        Some(crate::constants::MAX_NON_FINALIZED_INDEX_ENTRIES_TEST_NETWORKS),
        NonFinalizedState::new(&Network::new_default_testnet()).max_index_entries
    );
    const {
        assert!(
            crate::constants::MAX_NON_FINALIZED_INDEX_ENTRIES
                > crate::constants::MAX_NON_FINALIZED_INDEX_ENTRIES_TEST_NETWORKS
        )
    };
}

/// Past the index entry limit, Zebra finalizes blocks before the reorg limit, but keeps at least
/// `MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT` blocks.
#[test]
fn index_entry_limit_finalizes_early_down_to_the_minimum() -> Result<()> {
    let _init_guard = zebra_test::init();

    let min_blocks = crate::constants::MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT;
    for network in [Network::new_default_testnet(), Network::Mainnet] {
        let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let (mut state, finalized_state) = new_invalidate_test_state(&network);
        state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
        let mut tip = busy_child(&block1, &block1, &network, 1_000, 10);
        state.commit_block(tip.clone().prepare(), &finalized_state)?;
        while state.best_chain_len() < Some(min_blocks + 1) {
            tip = tip.make_fake_child();
            state.commit_block(tip.clone().prepare(), &finalized_state)?;
        }
        assert!(!state.should_finalize_root(), "{network}: under the limit");

        state.max_index_entries = Some(1);
        assert!(state.should_finalize_root(), "{network}: over the limit");

        while state.should_finalize_root() {
            state.finalize();
        }
        assert_eq!(Some(min_blocks), state.best_chain_len(), "{network}");
    }

    Ok(())
}

/// The chain's created output and address indexes share the block's outputs instead of copying
/// them.
#[test]
fn chain_indexes_share_block_outputs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, 100, 10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;

    // `new_outputs` can contain unrelated outputs, such as one past the end of a transaction.
    let coinbase = busy.transactions[0].hash();
    let past_end = transparent::OutPoint {
        hash: coinbase,
        index: u32::try_from(busy.transactions[0].outputs().len()).unwrap(),
    };
    let mut prepared = busy.clone().prepare();
    let unrelated = prepared.new_outputs.values().next().unwrap().clone();
    Arc::make_mut(&mut prepared.new_outputs).insert(past_end, unrelated);
    state.commit_block(prepared, &finalized_state)?;

    let chain = state.best_chain().unwrap();
    let block = chain.tip_block().unwrap();
    assert!(block.new_outputs.len() > 100);
    for (outpoint, utxo) in block.new_outputs.iter() {
        if *outpoint == past_end {
            continue;
        }
        assert!(Arc::ptr_eq(
            utxo,
            chain.created_ordered_utxo(outpoint).unwrap()
        ));
    }

    // The unrelated output isn't an output of the chain.
    assert!(chain.created_ordered_utxo(&past_end).is_none());

    Ok(())
}

/// Blocks restored into the non-finalized state count as sent, but the sent hashes don't copy
/// their outputs, which readers find in the restored state.
#[test]
fn sent_hashes_from_restored_blocks_hold_no_outputs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, 100, 10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(busy.clone().prepare(), &finalized_state)?;

    let sent = crate::service::queued_blocks::SentHashes::new(&state);
    let chain = state.best_chain().unwrap();
    for block in chain.blocks.values() {
        assert!(sent.contains(&block.hash));
        for outpoint in block.new_outputs.keys() {
            assert!(sent.utxo(outpoint).is_none());
            assert!(chain.created_utxo(outpoint).is_some());
        }
    }
    assert!(sent.can_fork_chain_at_hashes);

    Ok(())
}

/// Address indexes count the address outputs a block spends, and give the same results when a
/// query names more addresses than a block changes.
#[test]
fn address_indexes_track_spent_outputs() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let busy = busy_child(&block1, &block1, &network, 20, 4);

    // A block with a transaction that spends ten of the busy block's address outputs.
    let first_address_output = u32::try_from(block1.transactions[0].outputs().len()).unwrap();
    let spent: Vec<_> = (0..10)
        .map(|i| transparent::OutPoint {
            hash: busy.transactions[0].hash(),
            index: first_address_output + i * 2,
        })
        .collect();
    let spend = zebra_chain::transaction::Transaction::clone(&busy.transactions[0])
        .with_transparent_inputs(
            spent
                .iter()
                .map(|outpoint| transparent::Input::PrevOut {
                    outpoint: *outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: 0,
                })
                .collect(),
        )
        .with_transparent_outputs(vec![transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Address::from_pub_key_hash(network.kind(), [9; 20]).script(),
        }]);
    let mut spender = Block::clone(&busy.make_fake_child());
    spender.transactions.push(Arc::new(spend));
    let spender = Arc::new(spender);

    let mut chain = Chain::new(
        &network,
        (block1.coinbase_height().unwrap() - 1).unwrap(),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    chain = chain.push(block1.prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(busy.prepare().test_with_zero_spent_utxos())?;
    let spent_outputs = spent
        .iter()
        .map(|outpoint| {
            let utxo = chain
                .created_ordered_utxo(outpoint)
                .expect("the busy block created the spent outputs");
            (*outpoint, transparent::OrderedUtxo::clone(utxo))
        })
        .collect();
    let spender = crate::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        spender.prepare(),
        spent_outputs,
        zebra_chain::amount::DeferredPoolBalanceChange::zero(),
        &network,
        chain.chain_value_pools,
    )?;
    chain = chain.push(spender)?;

    let spent_addresses: std::collections::HashSet<_> = spent
        .iter()
        .map(|outpoint| {
            chain
                .created_ordered_utxo(outpoint)
                .unwrap()
                .utxo
                .output
                .address(&network)
                .unwrap()
        })
        .collect();
    assert_eq!(
        chain.partial_transparent_spent_utxo_count(&spent_addresses),
        10
    );
    crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes(&chain);

    Ok(())
}

/// Address balance reads leave out the blocks below a height, which the finalized state already
/// counts, without copying or changing the chain.
#[test]
fn address_balance_from_a_height_skips_lower_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    let chain = state.best_chain().unwrap();

    let coinbase_outputs = |block: &Block| block.transactions[0].outputs();
    let addresses: std::collections::HashSet<_> = coinbase_outputs(&block1)
        .iter()
        .chain(coinbase_outputs(&block2).iter())
        .filter_map(|output| output.address(&network))
        .collect();
    assert!(!addresses.is_empty());
    let value_to = |block: &Block| -> u64 {
        coinbase_outputs(block)
            .iter()
            .filter(|output| output.address(&network).is_some())
            .map(|output| u64::from(output.value()))
            .sum()
    };

    let root = chain.non_finalized_root_height();
    let tip = chain.non_finalized_tip_height();
    let (whole_balance, whole_received) = chain.partial_transparent_balance_change(&addresses);
    let (tip_balance, tip_received) =
        chain.partial_transparent_balance_change_from(&addresses, tip);
    assert_eq!(whole_received, value_to(&block1) + value_to(&block2));
    assert_eq!(tip_received, value_to(&block2));
    assert_eq!(
        whole_balance,
        chain
            .partial_transparent_balance_change_from(&addresses, root)
            .0
    );
    assert_eq!(
        i128::from(whole_balance.zatoshis()) - i128::from(tip_balance.zatoshis()),
        i128::from(value_to(&block1)),
    );

    Ok(())
}

/// Measures the cost of the chain copy that every commit and fork still makes, with
/// transaction-dense and input-dense blocks filling the window.
///
/// Run with `ZEBRA_TEST_MEASURE_MEMORY=1` and `--run-ignored=only --no-capture`.
/// `ZEBRA_TEST_TX_ENTRIES` sets the transactions, and spent outputs, in the window.
#[test]
#[ignore]
#[allow(clippy::print_stdout)]
fn measure_plain_index_clone_cost() {
    let _init_guard = zebra_test::init();
    if !memory_measurement_requested() {
        return;
    }

    let entries: u32 = std::env::var("ZEBRA_TEST_TX_ENTRIES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_500_000);
    let mib = |bytes: u64| bytes / (1024 * 1024);

    let start = resident_set_size();
    let mut chain = Chain::default();
    for i in 0..entries {
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&i.to_le_bytes());
        let hash = zebra_chain::transaction::Hash(hash);
        chain.tx_loc_by_hash.insert(
            hash,
            crate::TransactionLocation::from_usize(Height(i / 25_000), (i % 25_000) as usize),
        );
        #[cfg(feature = "indexer")]
        let spending = hash;
        #[cfg(not(feature = "indexer"))]
        let spending = ();
        chain
            .spent_utxos
            .insert(transparent::OutPoint { hash, index: 0 }, spending);
    }
    let filled = resident_set_size();

    let timer = std::time::Instant::now();
    let copy = chain.clone();
    let elapsed = timer.elapsed();
    let copied = resident_set_size();

    println!(
        "entries {entries}: RSS MiB start {}, filled {}, after one chain copy {}, copy took {:?}",
        mib(start),
        mib(filled),
        mib(copied),
        elapsed,
    );
    drop(copy);
}

/// Returns a chain of 8 blocks, and the `address_count` transparent addresses its transactions
/// send funds to.
///
/// Every coinbase pays each address, and every later block spends the two oldest outputs,
/// sending half of each one back to the same address, a quarter to the next address, and the
/// rest as fees.
fn transparent_transfers_chain(
    network: &Network,
    address_count: u8,
) -> Result<(Chain, Vec<transparent::Address>)> {
    use std::collections::VecDeque;

    use zebra_chain::{
        parameters::NetworkKind,
        transaction::{LockTime, Transaction},
    };

    let addresses: Vec<transparent::Address> = (1..=address_count)
        .map(|hash_byte| {
            if hash_byte % 3 == 0 {
                transparent::Address::from_script_hash(NetworkKind::Mainnet, [hash_byte; 20])
            } else {
                transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [hash_byte; 20])
            }
        })
        .collect();

    let template: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let first_height = template
        .coinbase_height()
        .expect("block 434873 has its height in its coinbase");

    let mut chain = Chain::new(
        network,
        (first_height - 1).expect("block 434873 is not the genesis block"),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    // The unspent outputs sent to `addresses`, as (outpoint, address index, value), oldest first.
    let mut unspent = VecDeque::new();
    let mut previous_block_hash = template.header.previous_block_hash;

    for block_index in 0..8 {
        let height = Height(first_height.0 + block_index);

        let coinbase_outputs = addresses
            .iter()
            .map(|address| {
                let value = Amount::try_from(1_000_000 * (i64::from(block_index) + 1))?;
                Ok(transparent::Output::new(value, address.script()))
            })
            .collect::<Result<_>>()?;
        let mut transactions = vec![Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            coinbase_outputs,
            LockTime::unlocked(),
            height,
        ))];

        if block_index > 0 {
            let spent: Vec<(transparent::OutPoint, usize, Amount<NonNegative>)> =
                unspent.drain(..2).collect();
            let inputs = spent
                .iter()
                .map(|(outpoint, _, _)| transparent::Input::PrevOut {
                    outpoint: *outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: u32::MAX,
                })
                .collect();
            let mut outputs = Vec::new();
            for (_, address_index, value) in &spent {
                let next_address_index = (address_index + 1) % addresses.len();
                outputs.push(transparent::Output::new(
                    (*value / 2)?,
                    addresses[*address_index].script(),
                ));
                outputs.push(transparent::Output::new(
                    (*value / 4)?,
                    addresses[next_address_index].script(),
                ));
            }
            transactions.push(Arc::new(Transaction::test_v4(
                inputs,
                outputs,
                LockTime::unlocked(),
                height,
            )));
        }

        for transaction in &transactions {
            let hash = transaction.hash();
            for (index, output) in transaction.outputs().iter().enumerate() {
                let address = output.address(network);
                let address_index = addresses
                    .iter()
                    .position(|candidate| Some(candidate) == address.as_ref())
                    .expect("all outputs pay one of the addresses");
                let outpoint = transparent::OutPoint {
                    hash,
                    index: index.try_into()?,
                };
                unspent.push_back((outpoint, address_index, output.value()));
            }
        }

        chain = push_transparent_block(
            chain,
            network,
            &template,
            &mut previous_block_hash,
            transactions,
        )?;
    }

    Ok((chain, addresses))
}

/// Returns `chain` with a new block containing `transactions`, using `template` for the rest of
/// the block, and updates `previous_block_hash` to the hash of the new block.
///
/// The first transaction must be a coinbase transaction for the next height of `chain`.
fn push_transparent_block(
    chain: Chain,
    network: &Network,
    template: &Block,
    previous_block_hash: &mut block::Hash,
    transactions: Vec<Arc<zebra_chain::transaction::Transaction>>,
) -> Result<Chain> {
    use zebra_chain::amount::DeferredPoolBalanceChange;

    use crate::ContextuallyVerifiedBlock;

    let mut block = template.clone();
    Arc::make_mut(&mut block.header).previous_block_hash = *previous_block_hash;
    block.transactions = transactions;
    let block = Arc::new(block);
    *previous_block_hash = block.hash();

    let block = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        block.prepare(),
        chain.unspent_utxos(),
        DeferredPoolBalanceChange::zero(),
        network,
        chain.chain_value_pools,
    )?;
    let chain = chain.push(block)?;

    Ok(chain)
}

/// Returns each of `addresses` on its own, all of them, and an address without any transfers.
fn transparent_address_sets(
    addresses: &[transparent::Address],
) -> Vec<std::collections::HashSet<transparent::Address>> {
    use std::collections::HashSet;

    use zebra_chain::parameters::NetworkKind;

    // `transparent_transfers_chain()` doesn't use zero hashes.
    let unused_address = transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [0; 20]);

    addresses
        .iter()
        .map(|address| HashSet::from([*address]))
        .chain([
            addresses.iter().cloned().collect(),
            HashSet::from([unused_address]),
        ])
        .collect()
}

/// A chain's transparent balance change from a height in the middle of the chain is the same as
/// the balance change of that chain after popping the blocks below that height.
///
/// Balance reads use this to skip the blocks of a shared chain that are already finalized,
/// instead of cloning the chain and popping them. Regression test for GHSA-hj54-2mfv-4xg8.
///
/// The skipped blocks are scanned once for all the requested addresses, so this test also uses
/// a chain where each coinbase transaction pays many of the requested addresses.
#[test]
fn transparent_balance_change_skips_blocks_below_min_height() -> Result<()> {
    use zebra_chain::amount::NegativeAllowed;

    let _init_guard = zebra_test::init();

    for address_count in [3, 100] {
        let (chain, addresses) = transparent_transfers_chain(&Network::Mainnet, address_count)?;
        let address_sets = transparent_address_sets(&addresses);
        let all_addresses = &address_sets[addresses.len()];

        let root = chain.non_finalized_root_height();
        let tip = chain.non_finalized_tip_height();
        let mut popped_chain = chain.clone();

        for min_height in root.0..=tip.0 {
            let min_height = Height(min_height);
            while popped_chain.non_finalized_root_height() < min_height {
                popped_chain.pop_root();
            }

            for address_set in &address_sets {
                assert_eq!(
                    chain.partial_transparent_balance_change_from(address_set, min_height),
                    popped_chain.partial_transparent_balance_change_from(address_set, min_height),
                    "skipping the blocks below {min_height:?} must match popping them, \
                     for {address_set:?}",
                );
            }
        }

        // The test must actually skip some balance changes.
        let full_change = chain.partial_transparent_balance_change_from(all_addresses, root);
        let skipped_change = chain.partial_transparent_balance_change_from(
            all_addresses,
            (root + 1).expect("valid height"),
        );
        assert_ne!(full_change, skipped_change);
        assert_ne!(skipped_change.0, Amount::<NegativeAllowed>::zero());

        // If all the blocks are skipped, there is no change.
        assert_eq!(
            chain.partial_transparent_balance_change_from(
                all_addresses,
                (tip + 1).expect("valid height")
            ),
            (Amount::zero(), 0),
        );
    }

    Ok(())
}

/// Combining the transparent balance changes of several addresses doesn't overflow, in any
/// address order, even if their changes in the whole chain and in the blocks it shares with the
/// finalized state are each larger than half the money supply.
///
/// Regression test for an amount overflow panic in an earlier fix for GHSA-hj54-2mfv-4xg8.
#[test]
fn transparent_balance_change_does_not_overflow_with_large_overlapping_transfers() -> Result<()> {
    use std::collections::HashSet;

    use zebra_chain::{
        amount::{NegativeAllowed, MAX_MONEY},
        parameters::NetworkKind,
        transaction::{self, LockTime, Transaction},
    };

    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let [miner, sender, receiver, final_receiver] = [1, 2, 3, 4].map(|hash_byte| {
        transparent::Address::from_pub_key_hash(NetworkKind::Mainnet, [hash_byte; 20])
    });

    // Two of these amounts are more than the money supply.
    let large_value = Amount::<NonNegative>::try_from(MAX_MONEY / 2 + 1)?;

    let coinbase = |height: Height, address: transparent::Address, value: Amount<NonNegative>| {
        Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output::new(value, address.script())],
            LockTime::unlocked(),
            height,
        ))
    };
    // Sends the first output of the spent transaction to `address`, without any fees.
    let transfer =
        |height: Height, spent_tx_hash: transaction::Hash, address: transparent::Address| {
            Arc::new(Transaction::test_v4(
                vec![transparent::Input::PrevOut {
                    outpoint: transparent::OutPoint {
                        hash: spent_tx_hash,
                        index: 0,
                    },
                    unlock_script: transparent::Script::new(&[]),
                    sequence: u32::MAX,
                }],
                vec![transparent::Output::new(large_value, address.script())],
                LockTime::unlocked(),
                height,
            ))
        };

    let template: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let first_height = template
        .coinbase_height()
        .expect("block 434873 has its height in its coinbase");
    let [sender_height, receiver_height, final_receiver_height] =
        [0, 1, 2].map(|offset| Height(first_height.0 + offset));

    let to_sender = coinbase(sender_height, sender, large_value);
    let to_receiver = transfer(receiver_height, to_sender.hash(), receiver);
    let to_final_receiver = transfer(final_receiver_height, to_receiver.hash(), final_receiver);

    // The chain value pools start empty, so the total stays below the money supply.
    let mut chain = Chain::new(
        &network,
        (first_height - 1).expect("block 434873 is not the genesis block"),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::zero(),
    );
    let mut previous_block_hash = template.header.previous_block_hash;

    for transactions in [
        vec![to_sender],
        vec![
            coinbase(receiver_height, miner, Amount::zero()),
            to_receiver,
        ],
        vec![
            coinbase(final_receiver_height, miner, Amount::zero()),
            to_final_receiver,
        ],
    ] {
        chain = push_transparent_block(
            chain,
            &network,
            &template,
            &mut previous_block_hash,
            transactions,
        )?;
    }

    // The sender's block has been finalized and removed from the chain. The receiver's block has
    // been finalized, but it is still in the chain.
    chain.pop_root();
    let min_height = final_receiver_height;

    let large_change = large_value.constrain::<NegativeAllowed>()?;
    let zero_change = Amount::<NegativeAllowed>::zero();
    let negative_large_change = (zero_change - large_change)?;
    let large_received = u64::from(large_value);

    let expected_changes = [
        (vec![sender], (zero_change, 0)),
        (vec![receiver], (negative_large_change, 0)),
        (vec![final_receiver], (large_change, large_received)),
        // The receiver's change after the shared block, plus the sender's change in the whole
        // chain, is less than minus the money supply.
        (vec![sender, receiver], (negative_large_change, 0)),
        (
            vec![sender, receiver, final_receiver],
            (zero_change, large_received),
        ),
    ];

    for (addresses, expected_change) in expected_changes {
        // A `HashSet`'s order depends on its random hash keys, so check new sets until each
        // address has been the first one.
        let mut first_addresses = HashSet::new();
        for _ in 0..1_000 {
            let address_set: HashSet<transparent::Address> = addresses.iter().copied().collect();
            first_addresses.insert(*address_set.iter().next().expect("sets are not empty"));

            assert_eq!(
                chain.partial_transparent_balance_change_from(&address_set, min_height),
                expected_change,
                "balance change for {address_set:?}, in that order",
            );

            if first_addresses.len() == addresses.len() {
                break;
            }
        }
        assert_eq!(
            first_addresses.len(),
            addresses.len(),
            "each address must be first in one of the checked sets",
        );
    }

    Ok(())
}

/// A chain's limited transparent UTXO and transaction ID queries return the first entries of
/// the unlimited queries, in chain order, without collecting the rest.
#[test]
fn transparent_address_queries_are_limited_to_max_entries() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (chain, addresses) = transparent_transfers_chain(&Network::Mainnet, 3)?;
    let address_sets = transparent_address_sets(&addresses);
    let all_addresses = &address_sets[addresses.len()];

    let root = chain.non_finalized_root_height();
    let tip = chain.non_finalized_tip_height();
    let height_ranges = [
        root..=tip,
        (root + 2).expect("valid height")..=tip,
        root..=(tip - 3).expect("valid height"),
        tip..=root,
    ];

    // The test must use more UTXOs and transactions than the limits.
    let (all_created, all_spent) =
        chain.partial_transparent_utxo_changes(all_addresses, &(root..=tip), None);
    assert!(all_created.len() > 6);
    assert!(!all_spent.is_empty());
    assert!(
        chain
            .partial_transparent_tx_ids(all_addresses, root..=tip, None)
            .len()
            > 6
    );

    for address_set in &address_sets {
        for height_range in &height_ranges {
            // The expected UTXOs are the unspent UTXOs in the range, like the query used to
            // return them before it was limited.
            let (unlimited_created, unlimited_spent) =
                chain.partial_transparent_utxo_changes(address_set, &(root..=tip), None);
            let expected_utxos: Vec<_> = unlimited_created
                .into_iter()
                .filter(|(out_loc, _output)| {
                    height_range.contains(&out_loc.height()) && !unlimited_spent.contains(out_loc)
                })
                .collect();
            let unlimited_tx_ids =
                chain.partial_transparent_tx_ids(address_set, height_range.clone(), None);

            for max_entries in [0, 1, 2, 5, usize::MAX] {
                let (created, spent) = chain.partial_transparent_utxo_changes(
                    address_set,
                    height_range,
                    Some(max_entries),
                );
                assert_eq!(
                    created.into_iter().collect::<Vec<_>>(),
                    expected_utxos
                        .iter()
                        .take(max_entries)
                        .cloned()
                        .collect::<Vec<_>>(),
                    "UTXOs for {address_set:?} in {height_range:?}, at most {max_entries}",
                );
                assert_eq!(spent, unlimited_spent);

                let tx_ids = chain.partial_transparent_tx_ids(
                    address_set,
                    height_range.clone(),
                    Some(max_entries),
                );
                assert_eq!(
                    tx_ids.into_iter().collect::<Vec<_>>(),
                    unlimited_tx_ids
                        .iter()
                        .take(max_entries)
                        .map(|(tx_loc, tx_hash)| (*tx_loc, *tx_hash))
                        .collect::<Vec<_>>(),
                    "transaction IDs for {address_set:?} in {height_range:?}, at most {max_entries}",
                );
            }
        }
    }

    Ok(())
}
