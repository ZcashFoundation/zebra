use std::{env, sync::Arc};

use zebra_test::prelude::*;

use zebra_chain::{block::Block, fmt::DisplayToDebug, parameters::NetworkUpgrade::*, LedgerState};

use crate::{
    service::{
        arbitrary::PreparedChain,
        finalized_state::FinalizedState,
        non_finalized_state::{Chain, NonFinalizedState},
    },
    tests::Prepare,
    Config,
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 16;

/// Check that a forked chain is the same as a chain that had the same blocks appended.
#[test]
fn forked_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, fork_at_count, _network) in PreparedChain::default())| {
            // use `fork_at_count` as the fork tip
            let fork_tip_hash = chain[fork_at_count - 1].hash;
            let mut full_chain = Chain::default();
            let mut partial_chain = Chain::default();

            for block in chain.iter().take(fork_at_count) {
                partial_chain = partial_chain.push(block.clone())?;
            }
            for block in chain.iter() {
                full_chain = full_chain.push(block.clone())?;
            }

            let forked = full_chain.fork(fork_tip_hash).expect("fork works").expect("hash is present");

            // the first check is redundant, but it's useful for debugging
            prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());
            prop_assert!(forked.eq_internal_state(&partial_chain));
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
    |((chain, end_count, _network) in PreparedChain::default())| {
        // use `end_count` as the number of non-finalized blocks at the end of the chain
        let finalized_count = chain.len() - end_count;
        let mut full_chain = Chain::default();
        let mut partial_chain = Chain::default();

        for block in chain.iter().skip(finalized_count) {
            partial_chain = partial_chain.push(block.clone())?;
        }
        for block in chain.iter() {
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
                .prop_flat_map(|((chain, valid_count, network), is_nu5, is_v5)| {
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

                  // use `valid_count` as the number of valid blocks before an invalid block
                  let valid_tip_height = chain[valid_count - 1].height;
                  let valid_tip_hash = chain[valid_count - 1].hash;
                  let mut chain = chain.iter().take(valid_count).cloned();

                  prop_assert!(state.eq_internal_state(&state));

                  if let Some(first_block) = chain.next() {
                      state.commit_new_chain(first_block, &finalized_state)?;
                      prop_assert!(state.eq_internal_state(&state));
                  }

                  for block in chain {
                      state.commit_block(block, &finalized_state)?;
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
                               .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((block1, block2) in (any::<bool>(), any::<bool>())
      .prop_flat_map(|(is_nu5, is_v5)| {
          // generate a Canopy or NU5 block with v4 or v5 transactions
          LedgerState::coinbase_strategy(
              if is_nu5 { Nu5 } else { Canopy },
              if is_nu5 && is_v5 { 5 } else { 4 },
              true,
          )})
      .prop_map(Block::arbitrary_with)
      .prop_flat_map(|block_strategy| (block_strategy.clone(), block_strategy))
      .prop_map(|(block1, block2)| (DisplayToDebug(block1), DisplayToDebug(block2)))
    )| {
        let chain1 = Chain::default();
        let chain2 = Chain::default();

        let block1 = Arc::new(block1.0).prepare();
        let block2 = Arc::new(block2.0).prepare();

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

                // anchors
                chain1.sprout_anchors = chain2.sprout_anchors.clone();
                chain1.sapling_anchors = chain2.sapling_anchors.clone();
                chain1.orchard_anchors = chain2.orchard_anchors.clone();

                // nullifiers
                chain1.sprout_nullifiers = chain2.sprout_nullifiers.clone();
                chain1.sapling_nullifiers = chain2.sapling_nullifiers.clone();
                chain1.orchard_nullifiers = chain2.orchard_nullifiers.clone();

                // proof of work
                chain1.partial_cumulative_work = chain2.partial_cumulative_work;

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
