use std::env;

use zebra_test::prelude::*;

use crate::service::non_finalized_state::{arbitrary::PreparedChain, Chain};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 32;

#[test]
fn forked_equals_pushed() -> Result<()> {
    zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count) in PreparedChain::default())| {
            let fork_tip_hash = chain[count - 1].hash;
            let mut full_chain = Chain::default();
            let mut partial_chain = Chain::default();

            for block in chain.iter().take(count) {
                partial_chain.push(block.clone());
            }
            for block in chain.iter() {
                full_chain.push(block.clone());
            }

            let forked = full_chain.fork(fork_tip_hash).expect("hash is present");

            prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());
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
    |((chain, end_count) in PreparedChain::default())| {
        let finalized_count = chain.len() - end_count;
        let mut full_chain = Chain::default();
        let mut partial_chain = Chain::default();

        for block in chain.iter().skip(finalized_count) {
            partial_chain.push(block.clone());
        }
        for block in chain.iter() {
            full_chain.push(block.clone());
        }

        for _ in 0..finalized_count {
            let _finalized = full_chain.pop_root();
        }

        prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());
    });

    Ok(())
}
