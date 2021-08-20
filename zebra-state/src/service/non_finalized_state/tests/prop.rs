use std::{env, sync::Arc};

use zebra_test::prelude::*;

use zebra_chain::{
    amount::NonNegative,
    block::{self, arbitrary::allow_all_transparent_coinbase_spends, Block},
    fmt::DisplayToDebug,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::NetworkUpgrade::*,
    parameters::{Network, *},
    value_balance::ValueBalance,
    LedgerState,
};

use crate::{
    arbitrary::Prepare,
    request::ContextuallyValidBlock,
    service::{
        arbitrary::PreparedChain,
        finalized_state::FinalizedState,
        non_finalized_state::{Chain, NonFinalizedState},
    },
    Config,
};

/// The default number of proptest cases for long partial chain tests.
const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 1;

/// The default number of proptest cases for short partial chain tests.
const DEFAULT_SHORT_CHAIN_PROPTEST_CASES: u32 = 16;

/// Check that a forked chain is the same as a chain that had the same blocks appended.
///
/// Also check for:
/// - no transparent spends in the genesis block, because genesis transparent outputs are ignored
#[test]
fn forked_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, fork_at_count, network, finalized_tree) in PreparedChain::new_heartwood())| {
            // Skip first block which was used for the history tree; make sure fork_at_count is still valid
            let fork_at_count = std::cmp::min(fork_at_count, chain.len() - 1);
            let chain = &chain[1..];
            // use `fork_at_count` as the fork tip
            let fork_tip_hash = chain[fork_at_count - 1].hash;

            let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
            let mut full_chain = Chain::new(network, Default::default(), Default::default(), finalized_tree.clone(), fake_value_pool);
            let mut partial_chain = Chain::new(network, Default::default(), Default::default(), finalized_tree.clone(), fake_value_pool);

            for block in chain
                .iter()
                .take(fork_at_count)
                .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                    partial_chain = partial_chain.push(block)?;
                }

            for block in chain
                .iter()
                .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                    full_chain = full_chain.push(block.clone())?;

                    // check some other properties of generated chains
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
                            "unexpected transparent prevout input at height {:?}: \
                             genesis transparent outputs must be ignored, \
                             so there can not be any spends in the genesis block",
                            block.height,
                        );
                    }
                }

            let mut forked = full_chain
                .fork(
                    fork_tip_hash,
                    Default::default(),
                    Default::default(),
                    finalized_tree,
                )
                .expect("fork works")
                .expect("hash is present");

            // the first check is redundant, but it's useful for debugging
            prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());
            prop_assert!(forked.eq_internal_state(&partial_chain));

            // Re-add blocks to the fork and check if we arrive at the
            // same original full chain
            for block in chain
                .iter()
                .skip(fork_at_count)
                .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                    forked = forked.push(block)?;
            }

            prop_assert_eq!(forked.blocks.len(), full_chain.blocks.len());
            prop_assert!(forked.eq_internal_state(&full_chain));
        });

    Ok(())
}

/// Check that a chain with some blocks finalized is the same as
/// a chain that never had those blocks added.
#[test]
fn finalized_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                      .ok()
                                      .and_then(|v| v.parse().ok())
                                      .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, end_count, network, finalized_tree) in PreparedChain::new_heartwood())| {
        // Skip first block which was used for the history tree; make sure end_count is still valid
        let end_count = std::cmp::min(end_count, chain.len() - 1);
        let chain = &chain[1..];
        // use `end_count` as the number of non-finalized blocks at the end of the chain
        let finalized_count = chain.len() - end_count;

        let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();

        let mut full_chain = Chain::new(network, Default::default(), Default::default(), finalized_tree, fake_value_pool);
        for block in chain
            .iter()
            .take(finalized_count)
            .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                full_chain = full_chain.push(block)?;
            }

        let mut partial_chain = Chain::new(
            network,
            full_chain.sapling_note_commitment_tree.clone(),
            full_chain.orchard_note_commitment_tree.clone(),
            full_chain.history_tree.clone(),
            full_chain.value_balance,
        );
        for block in chain
            .iter()
            .skip(finalized_count)
            .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                partial_chain = partial_chain.push(block.clone())?;
            }

        for block in chain
            .iter()
            .skip(finalized_count)
            .map(ContextuallyValidBlock::test_with_zero_chain_pool_change) {
                full_chain = full_chain.push(block.clone())?;
            }

        for _ in 0..finalized_count {
            let _finalized = full_chain.pop_root();
        }

        prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());
        prop_assert!(full_chain.eq_internal_state(&partial_chain));
    });

    Ok(())
}

/// Check that rejected blocks do not change the internal state of a chain
/// in a non-finalized state.
#[test]
fn rejection_restores_internal_state() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                         .ok()
                                         .and_then(|v| v.parse().ok())
                                         .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
              |((chain, valid_count, network, mut bad_block) in (PreparedChain::default(), any::<bool>(), any::<bool>())
                .prop_flat_map(|((chain, valid_count, network, _history_tree), is_nu5, is_v5)| {
                    let next_height = chain[valid_count - 1].height;
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
                  let mut state = NonFinalizedState::new(network);
                  let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

                  let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
                  finalized_state.set_current_value_pool(fake_value_pool);

                  // use `valid_count` as the number of valid blocks before an invalid block
                  let valid_tip_height = chain[valid_count - 1].height;
                  let valid_tip_hash = chain[valid_count - 1].hash;
                  let mut chain = chain.iter().take(valid_count).cloned();

                  prop_assert!(state.eq_internal_state(&state));

                  if let Some(first_block) = chain.next() {
                      let result = state.commit_new_chain(first_block, &finalized_state);
                      prop_assert_eq!(
                          result,
                          Ok(()),
                          "PreparedChain should generate a valid first block"
                      );
                      prop_assert!(state.eq_internal_state(&state));
                  }

                  for block in chain {
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

                  bad_block.header.previous_block_hash = valid_tip_hash;
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
    zebra_test::init();

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
      .prop_map(|ledger_state| Block::partial_chain_strategy(ledger_state, 2, allow_all_transparent_coinbase_spends))
      .prop_flat_map(|block_strategy| (block_strategy.clone(), block_strategy))
    )| {
        let prev_block1 = vec1[0].clone();
        let prev_block2 = vec2[0].clone();
        let height1 = prev_block1.coinbase_height().unwrap();
        let height2 = prev_block1.coinbase_height().unwrap();
        let finalized_tree1: HistoryTree = if height1 >= Heartwood.activation_height(Network::Mainnet).unwrap() {
            NonEmptyHistoryTree::from_block(Network::Mainnet, prev_block1, &Default::default(), &Default::default()).unwrap().into()
        } else {
            Default::default()
        };
        let finalized_tree2 = if height2 >= NetworkUpgrade::Heartwood.activation_height(Network::Mainnet).unwrap() {
            NonEmptyHistoryTree::from_block(Network::Mainnet, prev_block2, &Default::default(), &Default::default()).unwrap().into()
        } else {
            Default::default()
        };
        let chain1 = Chain::new(Network::Mainnet, Default::default(), Default::default(), finalized_tree1, ValueBalance::fake_populated_pool());
        let chain2 = Chain::new(Network::Mainnet, Default::default(), Default::default(), finalized_tree2, ValueBalance::fake_populated_pool());

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
                chain1.height_by_hash = chain2.height_by_hash.clone();
                chain1.tx_by_hash = chain2.tx_by_hash.clone();

                // transparent UTXOs
                chain1.created_utxos = chain2.created_utxos.clone();
                chain1.spent_utxos = chain2.spent_utxos.clone();

                // note commitment trees
                chain1.sapling_note_commitment_tree = chain2.sapling_note_commitment_tree.clone();
                chain1.orchard_note_commitment_tree = chain2.orchard_note_commitment_tree.clone();

                // history tree
                chain1.history_tree = chain2.history_tree.clone();

                // anchors
                chain1.sapling_anchors = chain2.sapling_anchors.clone();
                chain1.sapling_anchors_by_height = chain2.sapling_anchors_by_height.clone();
                chain1.orchard_anchors = chain2.orchard_anchors.clone();
                chain1.orchard_anchors_by_height = chain2.orchard_anchors_by_height.clone();

                // nullifiers
                chain1.sprout_nullifiers = chain2.sprout_nullifiers.clone();
                chain1.sapling_nullifiers = chain2.sapling_nullifiers.clone();
                chain1.orchard_nullifiers = chain2.orchard_nullifiers.clone();

                // proof of work
                chain1.partial_cumulative_work = chain2.partial_cumulative_work;

                // chain value pool
                chain1.value_balance = chain2.value_balance;

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
