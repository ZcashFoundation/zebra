use std::env;

use zebra_chain::{history_tree::HistoryTree, sapling};
use zebra_test::prelude::*;

use crate::service::{arbitrary::PreparedChain, non_finalized_state::Chain};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 32;

#[test]
fn forked_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network) in PreparedChain::new_heartwood())| {
            // Build a history tree with the first block to simulate the tree of
            // the finalized state.
            let finalized_tree = HistoryTree::from_block(network, chain[0].block.clone(), &sapling::tree::Root::default(), None).unwrap();
            let chain = &chain[1..];
            let fork_tip_hash = chain[count - 1].hash;
            let mut full_chain = Chain::new(finalized_tree.clone());
            let mut partial_chain = Chain::new(finalized_tree.clone());

            for block in chain.iter().take(count) {
                partial_chain.push(block.clone())?;
            }
            for block in chain.iter() {
                full_chain.push(block.clone())?;
            }

            let mut forked = full_chain.fork(fork_tip_hash, &finalized_tree).expect("fork works").expect("hash is present");

            prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());
            prop_assert!(forked.is_identical(&partial_chain));

            for block in chain.iter().skip(count) {
                forked.push(block.clone())?;
            }

            prop_assert_eq!(forked.blocks.len(), full_chain.blocks.len());
            prop_assert!(forked.is_identical(&full_chain));
        });

    Ok(())
}

#[test]
fn finalized_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                      .ok()
                                      .and_then(|v| v.parse().ok())
                                      .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
    |((chain, end_count, network) in PreparedChain::new_heartwood())| {
        // Build a history tree with the first block to simulate the tree of
        // the finalized state.
        let finalized_tree = HistoryTree::from_block(network, chain[0].block.clone(), &sapling::tree::Root::default(), None).unwrap();
        let chain = &chain[1..];
        let finalized_count = chain.len() - end_count;
        let mut full_chain = Chain::new(finalized_tree);

        for block in chain.iter().take(finalized_count) {
            full_chain.push(block.clone())?;
        }
        let mut partial_chain = Chain::new(full_chain.history_tree.clone());
        for block in chain.iter().skip(finalized_count) {
            full_chain.push(block.clone())?;
        }

        for block in chain.iter().skip(finalized_count) {
            partial_chain.push(block.clone())?;
        }

        for _ in 0..finalized_count {
            let _finalized = full_chain.pop_root();
        }

        prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());
        prop_assert!(full_chain.is_identical(&partial_chain));
    });

    Ok(())
}
