//! Randomised property tests for the non-finalized state.

use std::{collections::BTreeMap, env, sync::Arc};

use zebra_test::prelude::*;

use zebra_chain::{
    amount::NonNegative,
    block::{self, arbitrary::allow_all_transparent_coinbase_spends, Block, Height},
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::NetworkUpgrade::*,
    parameters::*,
    value_balance::ValueBalance,
    LedgerState,
};

use crate::{
    arbitrary::Prepare,
    request::ContextuallyVerifiedBlock,
    service::{arbitrary::PreparedChain, non_finalized_state::Chain},
};

/// The default number of proptest cases for long partial chain tests.
const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 1;

/// The default number of proptest cases for short partial chain tests.
const DEFAULT_SHORT_CHAIN_PROPTEST_CASES: u32 = 16;

/// Check that chain block pushes work with blocks from genesis
///
/// Logs extra debugging information when the chain value balances fail.
#[test]
fn push_genesis_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
    ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, count, network, empty_tree) in PreparedChain::default())| {
        prop_assert!(empty_tree.is_none());

        let mut only_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), empty_tree, ValueBalance::zero());
        // contains the block value pool changes and chain value pool balances for each height
        let mut chain_values = BTreeMap::new();

        chain_values.insert(None, (None, only_chain.chain_value_pools.into()));

        for block in chain.iter().take(count).skip(1).cloned() {
            let block =
            ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                    block,
                    only_chain.unspent_utxos(),
                    #[cfg(feature = "tx-v6")]
                    Default::default(),
                )
                .map_err(|e| (e, chain_values.clone()))
                .expect("invalid block value pool change");

            chain_values.insert(block.height.into(), (block.chain_value_pool_change.into(), None));

            only_chain = only_chain
                .push(block.clone())
                .map_err(|e| (e, chain_values.clone()))
                .expect("invalid chain value pools");

            chain_values.insert(block.height.into(), (block.chain_value_pool_change.into(), only_chain.chain_value_pools.into()));
        }

        prop_assert_eq!(only_chain.blocks.len(), count - 1);
    });

    Ok(())
}

/// Check that chain block pushes work with history tree blocks
#[test]
fn push_history_tree_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
    ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, count, network, finalized_tree) in PreparedChain::new_heartwood())| {
        prop_assert!(finalized_tree.is_some());

        // Skip first block which was used for the history tree.
        // This skips some transactions which are required to calculate value balances,
        // so we zero all transparent inputs in this test.

        // make sure count is still valid
        let count = std::cmp::min(count, chain.len() - 1);
        let chain = &chain[1..];

        let mut only_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree, ValueBalance::zero());

        for block in chain
            .iter()
            .take(count)
            .map(ContextuallyVerifiedBlock::test_with_zero_chain_pool_change) {
                only_chain = only_chain.push(block)?;
            }

        prop_assert_eq!(only_chain.blocks.len(), count);
    });

    Ok(())
}

/// Checks that a forked genesis chain is the same as a chain that had the same
/// blocks appended.
///
/// In other words, this test checks that we get the same chain if we:
/// - fork the original chain, then push some blocks, or
/// - push the same blocks to the original chain.
///
/// Also checks that:
/// - There are no transparent spends in the chain from the genesis block,
///   because genesis transparent outputs are ignored.
/// - Transactions only spend transparent outputs from earlier in the block or
///   chain.
/// - Chain value balances are non-negative.
#[test]
fn forked_equals_pushed_genesis() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
    ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, fork_at_count, network, empty_tree) in PreparedChain::default())| {
        prop_assert!(empty_tree.is_none());

        // This chain will be used to check if the blocks in the forked chain
        // correspond to the blocks in the original chain before the fork.
        let mut partial_chain = Chain::new(
            &network,
            Height(0),
            Default::default(),
            Default::default(),
            Default::default(),
            empty_tree.clone(),
            ValueBalance::zero(),
        );
        for block in chain.iter().take(fork_at_count).skip(1).cloned() {
            let block = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                block,
                partial_chain.unspent_utxos(),
                #[cfg(feature = "tx-v6")]
                Default::default()
            )?;
            partial_chain = partial_chain
                .push(block)
                .expect("partial chain push is valid");
        }

        // This chain will be forked.
        let mut full_chain = Chain::new(
            &network,
            Height(0),
            Default::default(),
            Default::default(),
            Default::default(),
            empty_tree,
            ValueBalance::zero(),
        );

        for block in chain.iter().cloned() {
            let block = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
                block,
                full_chain.unspent_utxos(),
                #[cfg(feature = "tx-v6")]
                Default::default()
            )?;

            // Check some properties of the genesis block and don't push it to the chain.
            if block.height == block::Height(0) {
                prop_assert_eq!(
                    block
                        .block
                        .transactions
                        .iter()
                        .flat_map(|t| t.inputs())
                        .filter_map(|i| i.outpoint())
                        .count(),
                    0,
                    "Unexpected transparent prevout input at height 0. Genesis transparent outputs \
                        must be ignored, so there can not be any spends in the genesis block.",
                );
            } else {
                full_chain = full_chain
                    .push(block)
                    .expect("full chain push is valid");
            }
        }

        // Use [`fork_at_count`] as the fork tip.
        let fork_tip_height = fork_at_count - 1;
        let fork_tip_hash = chain[fork_tip_height].hash;

        // Fork the chain.
        let mut forked = full_chain
            .fork(fork_tip_hash)
            .expect("hash is present");

        // This check is redundant, but it's useful for debugging.
        prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());

        // Check that the entire internal state of the forked chain corresponds to the state of
        // the original chain.
        prop_assert!(forked.eq_internal_state(&partial_chain));

        // Re-add blocks to the fork and check if we arrive at the
        // same original full chain.
        for block in chain.iter().skip(fork_at_count).cloned() {
            let block =
            ContextuallyVerifiedBlock::with_block_and_spent_utxos(block, forked.unspent_utxos(),
            #[cfg(feature = "tx-v6")]
            Default::default())?;
            forked = forked.push(block).expect("forked chain push is valid");
        }

        prop_assert_eq!(forked.blocks.len(), full_chain.blocks.len());
        prop_assert!(forked.eq_internal_state(&full_chain));
    });

    Ok(())
}

/// Check that a forked history tree chain is the same as a chain that had the same blocks appended.
#[test]
fn forked_equals_pushed_history_tree() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
    ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, fork_at_count, network, finalized_tree) in PreparedChain::new_heartwood())| {
        prop_assert!(finalized_tree.is_some());

        // Skip first block which was used for the history tree.
        // This skips some transactions which are required to calculate value balances,
        // so we zero all transparent inputs in this test.

        // make sure fork_at_count is still valid
        let fork_at_count = std::cmp::min(fork_at_count, chain.len() - 1);
        let chain = &chain[1..];
        // use `fork_at_count` as the fork tip
        let fork_tip_hash = chain[fork_at_count - 1].hash;

        let mut full_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree.clone(), ValueBalance::zero());
        let mut partial_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree, ValueBalance::zero());

        for block in chain
            .iter()
            .take(fork_at_count)
            .map(ContextuallyVerifiedBlock::test_with_zero_chain_pool_change) {
                partial_chain = partial_chain.push(block)?;
            }

        for block in chain
            .iter()
            .map(ContextuallyVerifiedBlock::test_with_zero_chain_pool_change) {
                full_chain = full_chain.push(block.clone())?;
            }

        let mut forked = full_chain
            .fork(fork_tip_hash)
            .expect("hash is present");

        // the first check is redundant, but it's useful for debugging
        prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());
        prop_assert!(forked.eq_internal_state(&partial_chain));

        // Re-add blocks to the fork and check if we arrive at the
        // same original full chain
        for block in chain
            .iter()
            .skip(fork_at_count)
            .map(ContextuallyVerifiedBlock::test_with_zero_chain_pool_change) {
                forked = forked.push(block)?;
        }

        prop_assert_eq!(forked.blocks.len(), full_chain.blocks.len());
        prop_assert!(forked.eq_internal_state(&full_chain));
    });

    Ok(())
}

/// Check that a genesis chain with some blocks finalized is the same as
/// a chain that never had those blocks added.
#[test]
fn finalized_equals_pushed_genesis() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                         .ok()
                                         .and_then(|v| v.parse().ok())
                                         .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, end_count, network, empty_tree) in PreparedChain::default())| {

        // This test starts a partial chain from the middle of `chain`,
        // so it doesn't have the unspent UTXOs needed to calculate value balances.

        prop_assert!(empty_tree.is_none());

        // TODO: fix this test or the code so the full_chain temporary trees aren't overwritten
        let chain = chain.iter()
            .filter(|block| block.height != Height(0))
            .map(ContextuallyVerifiedBlock::test_with_zero_spent_utxos);

        // use `end_count` as the number of non-finalized blocks at the end of the chain,
        // make sure this test pushes at least 1 block in the partial chain.
        let finalized_count = 1.max(chain.clone().count() - end_count);

        let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();

        let mut full_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), empty_tree, fake_value_pool);
        for block in chain
            .clone()
            .take(finalized_count) {
                full_chain = full_chain.push(block)?;
            }

        let mut partial_chain = Chain::new(
            &network,
            full_chain.non_finalized_tip_height(),
            full_chain.sprout_note_commitment_tree_for_tip(),
            full_chain.sapling_note_commitment_tree_for_tip(),
            full_chain.orchard_note_commitment_tree_for_tip(),
            full_chain.history_block_commitment_tree(),
            full_chain.chain_value_pools,
        );
        for block in chain
            .clone()
            .skip(finalized_count) {
                partial_chain = partial_chain.push(block.clone())?;
            }

        for block in chain
            .skip(finalized_count) {
                full_chain = full_chain.push(block.clone())?;
            }

        for _ in 0..finalized_count {
            full_chain.pop_root();
        }

        // Make sure the temporary trees from finalized tip forks are removed.
        // TODO: update the test or the code so this extra step isn't needed?
        full_chain.pop_root();
        partial_chain.pop_root();

        prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());
        prop_assert!(
            full_chain.eq_internal_state(&partial_chain),
            "\n\
             full chain:\n{full_chain:#?}\n\n\
             partial chain:\n{partial_chain:#?}\n",
        );
    });

    Ok(())
}

/// Check that a history tree chain with some blocks finalized is the same as
/// a chain that never had those blocks added.
#[test]
fn finalized_equals_pushed_history_tree() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                         .ok()
                                         .and_then(|v| v.parse().ok())
                                         .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, end_count, network, finalized_tree) in PreparedChain::new_heartwood())| {


        prop_assert!(finalized_tree.is_some());

        // Skip first block which was used for the history tree; make sure end_count is still valid
        //
        // This skips some transactions which are required to calculate value balances,
        // so we zero all transparent inputs in this test.
        //
        // This test also starts a partial chain from the middle of `chain`,
        // so it doesn't have the unspent UTXOs needed to calculate value balances.
        let end_count = std::cmp::min(end_count, chain.len() - 1);
        let chain = &chain[1..];
        // use `end_count` as the number of non-finalized blocks at the end of the chain
        let finalized_count = chain.len() - end_count;

        let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();

        let mut full_chain = Chain::new(&network, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree, fake_value_pool);
        for block in chain
            .iter()
            .take(finalized_count)
            .map(ContextuallyVerifiedBlock::test_with_zero_spent_utxos) {
                full_chain = full_chain.push(block)?;
            }

        let mut partial_chain = Chain::new(
            &network,
            Height(finalized_count.try_into().unwrap()),
            full_chain.sprout_note_commitment_tree_for_tip(),
            full_chain.sapling_note_commitment_tree_for_tip(),
            full_chain.orchard_note_commitment_tree_for_tip(),
            full_chain.history_block_commitment_tree(),
            full_chain.chain_value_pools,
        );

        for block in chain
            .iter()
            .skip(finalized_count)
            .map(ContextuallyVerifiedBlock::test_with_zero_spent_utxos) {
                partial_chain = partial_chain.push(block.clone())?;
            }

        for block in chain
            .iter()
            .skip(finalized_count)
            .map(ContextuallyVerifiedBlock::test_with_zero_spent_utxos) {
                full_chain= full_chain.push(block.clone())?;
            }

        for _ in 0..finalized_count {
            full_chain.pop_root();
        }

        // Make sure the temporary trees from finalized tip forks are removed.
        // TODO: update the test or the code so this extra step isn't needed?
        full_chain.pop_root();
        partial_chain.pop_root();

        prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());
        prop_assert!(
            full_chain.eq_internal_state(&partial_chain),
            "\n\
             full chain:\n{full_chain:#?}\n\n\
             partial chain:\n{partial_chain:#?}\n",
        );
    });

    Ok(())
}

/// Check that rejected blocks do not change the internal state of a genesis chain
/// in a non-finalized state.
#[test]
#[cfg(not(target_os = "windows"))]
fn rejection_restores_internal_state_genesis() -> Result<()> {
    use zebra_chain::fmt::DisplayToDebug;

    use crate::{
        service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
        Config,
    };

    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, valid_count, network, mut bad_block) in (PreparedChain::default(), any::<bool>(), any::<bool>())
      .prop_flat_map(|((chain, valid_count, network, _history_tree), is_nu5, is_v5)| {
          let next_height = chain[valid_count].height;
          (
              Just(chain),
              Just(valid_count),
              Just(network),
              // generate a Canopy or NU5 block with v4 or v5 transactions
              LedgerState::height_strategy(
                  next_height,
                  if is_nu5 { Nu5 } else { Canopy },
                  if is_nu5 && is_v5 { 5 } else { 4 },
                  true,
              )
                  .prop_flat_map(Block::arbitrary_with)
                  .prop_map(DisplayToDebug)
          )
      }
      ))| {
        let mut state = NonFinalizedState::new(&network);
        let finalized_state = FinalizedState::new(&Config::ephemeral(), &network, #[cfg(feature = "elasticsearch")] false);

        let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
        finalized_state.set_finalized_value_pool(fake_value_pool);

        // use `valid_count` as the number of valid blocks before an invalid block
        let valid_tip_height = chain[valid_count - 1].height;
        let valid_tip_hash = chain[valid_count - 1].hash;
        let mut chain = chain.iter().take(valid_count).skip(1).cloned();

        prop_assert!(state.eq_internal_state(&state));

        if let Some(first_block) = chain.next() {
            // Allows anchor checks to pass
            finalized_state.populate_with_anchors(&first_block.block);

            let result = state.commit_new_chain(first_block, &finalized_state);
            prop_assert_eq!(
                result,
                Ok(()),
                "PreparedChain should generate a valid first block"
            );
            prop_assert!(state.eq_internal_state(&state));
        }

        for block in chain {
            // Allows anchor checks to pass
            finalized_state.populate_with_anchors(&block.block);

            let result = state.commit_block(block.clone(), &finalized_state);
            prop_assert_eq!(
                result,
                Ok(()),
                "PreparedChain should generate a valid block at {:?}",
                block.height,
            );
            prop_assert!(state.eq_internal_state(&state));
        }

        prop_assert_eq!(state.best_tip(), Some((valid_tip_height, valid_tip_hash)));

        let mut reject_state = state.clone();
        // the tip check is redundant, but it's useful for debugging
        prop_assert_eq!(state.best_tip(), reject_state.best_tip());
        prop_assert!(state.eq_internal_state(&reject_state));

        Arc::make_mut(&mut bad_block.header).previous_block_hash = valid_tip_hash;
        let bad_block = Arc::new(bad_block.0).prepare();
        let reject_result = reject_state.commit_block(bad_block, &finalized_state);

        if reject_result.is_err() {
            prop_assert_eq!(state.best_tip(), reject_state.best_tip());
            prop_assert!(state.eq_internal_state(&reject_state));
        } else {
            // the block just happened to pass all the non-finalized checks
            prop_assert_ne!(state.best_tip(), reject_state.best_tip());
            prop_assert!(!state.eq_internal_state(&reject_state));
        }
    });

    Ok(())
}

/// Check that different blocks create different internal chain states,
/// and that all the state fields are covered by `eq_internal_state`.
#[test]
fn different_blocks_different_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_SHORT_CHAIN_PROPTEST_CASES)),
    |((vec1, vec2) in (any::<bool>(), any::<bool>())
      .prop_flat_map(|(is_nu5, is_v5)| {
          // generate a Canopy or NU5 block with v4 or v5 transactions
          LedgerState::coinbase_strategy(
              if is_nu5 { Nu5 } else { Canopy },
              if is_nu5 && is_v5 { 5 } else { 4 },
              true,
          )})
      .prop_map(|ledger_state| Block::partial_chain_strategy(ledger_state, 2, allow_all_transparent_coinbase_spends, false))
      .prop_flat_map(|block_strategy| (block_strategy.clone(), block_strategy))
    )| {
        let prev_block1 = vec1[0].clone();
        let prev_block2 = vec2[0].clone();

        let height1 = prev_block1.coinbase_height().unwrap();
        let height2 = prev_block1.coinbase_height().unwrap();

        let finalized_tree1: Arc<HistoryTree> = if height1 >= Heartwood.activation_height(&Network::Mainnet).unwrap() {
            Arc::new(
                NonEmptyHistoryTree::from_block(&Network::Mainnet, prev_block1, &Default::default(), &Default::default()).unwrap().into()
            )
        } else {
            Default::default()
        };
        let finalized_tree2: Arc<HistoryTree> = if height2 >= NetworkUpgrade::Heartwood.activation_height(&Network::Mainnet).unwrap() {
            Arc::new(
                NonEmptyHistoryTree::from_block(&Network::Mainnet, prev_block2, &Default::default(), &Default::default()).unwrap().into()
            )
        } else {
            Default::default()
        };

        let chain1 = Chain::new(&Network::Mainnet, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree1, ValueBalance::fake_populated_pool());
        let chain2 = Chain::new(&Network::Mainnet, Height(0), Default::default(), Default::default(), Default::default(), finalized_tree2, ValueBalance::fake_populated_pool());

        let block1 = vec1[1].clone().prepare().test_with_zero_spent_utxos();
        let block2 = vec2[1].clone().prepare().test_with_zero_spent_utxos();

        let result1 = chain1.push(block1.clone());
        let result2 = chain2.push(block2.clone());

        // if there is an error, we don't get the chains back
        if let (Ok(mut chain1), Ok(chain2)) = (result1, result2) {
            if block1 == block2 {
                // the blocks were equal, so the chains should be equal

                // the first check is redundant, but it's useful for debugging
                prop_assert_eq!(&chain1.height_by_hash, &chain2.height_by_hash);
                prop_assert!(chain1.eq_internal_state(&chain2));
            } else {
                // the blocks were different, so the chains should be different

                prop_assert_ne!(&chain1.height_by_hash, &chain2.height_by_hash);
                prop_assert!(!chain1.eq_internal_state(&chain2));

                // We can't derive eq_internal_state,
                // so we check for missing fields here.

                // blocks, heights, hashes
                chain1.blocks = chain2.blocks.clone();
                chain1.height_by_hash.clone_from(&chain2.height_by_hash);
                chain1.tx_loc_by_hash.clone_from(&chain2.tx_loc_by_hash);

                // transparent UTXOs
                chain1.created_utxos.clone_from(&chain2.created_utxos);
                chain1.spent_utxos.clone_from(&chain2.spent_utxos);

                // note commitment trees
                chain1.sprout_trees_by_anchor.clone_from(&chain2.sprout_trees_by_anchor);
                chain1.sprout_trees_by_height = chain2.sprout_trees_by_height.clone();
                chain1.sapling_trees_by_height = chain2.sapling_trees_by_height.clone();
                chain1.orchard_trees_by_height = chain2.orchard_trees_by_height.clone();

                // note commitment subtrees
                chain1.sapling_subtrees = chain2.sapling_subtrees.clone();
                chain1.orchard_subtrees = chain2.orchard_subtrees.clone();

                // history trees
                chain1.history_trees_by_height = chain2.history_trees_by_height.clone();

                // anchors
                chain1.sprout_anchors = chain2.sprout_anchors.clone();
                chain1.sprout_anchors_by_height = chain2.sprout_anchors_by_height.clone();
                chain1.sapling_anchors = chain2.sapling_anchors.clone();
                chain1.sapling_anchors_by_height = chain2.sapling_anchors_by_height.clone();
                chain1.orchard_anchors = chain2.orchard_anchors.clone();
                chain1.orchard_anchors_by_height = chain2.orchard_anchors_by_height.clone();

                // nullifiers
                chain1.sprout_nullifiers.clone_from(&chain2.sprout_nullifiers);
                chain1.sapling_nullifiers.clone_from(&chain2.sapling_nullifiers);
                chain1.orchard_nullifiers.clone_from(&chain2.orchard_nullifiers);

                // proof of work
                chain1.partial_cumulative_work = chain2.partial_cumulative_work;

                // chain value pool
                chain1.chain_value_pools = chain2.chain_value_pools;

                // If this check fails, the `Chain` fields are out
                // of sync with `eq_internal_state` or this test.
                prop_assert!(
                    chain1.eq_internal_state(&chain2),
                    "Chain fields, eq_internal_state, and this test must be consistent"
                );
            }
        }
    });

    Ok(())
}
