//! Randomised property tests for the finalized state.

use std::env;

use zebra_chain::{block::Height, parameters::NetworkUpgrade};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::{
        arbitrary::PreparedChain,
        finalized_state::{FinalizedBlock, FinalizedState},
    },
    tests::FakeChainHelper,
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 1;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    let _init_guard = zebra_test::init();
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network, _history_tree) in PreparedChain::default())| {
            let mut state = FinalizedState::new(&Config::ephemeral(), network);
            let mut height = Height(0);
            // use `count` to minimize test failures, so they are easier to diagnose
            for block in chain.iter().take(count) {
                let hash = state.commit_finalized_direct(
                    FinalizedBlock::from(block.block.clone()),
                    "blocks_with_v5_transactions test"
                );
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                prop_assert_eq!(hash.unwrap(), block.hash);
                // TODO: check that the nullifiers were correctly inserted (#2230)
                height = Height(height.0 + 1);
            }
    });

    Ok(())
}

/// Test if committing blocks from all upgrades work correctly, to make
/// sure the contextual validation done by the finalized state works.
/// Also test if a block with the wrong commitment is correctly rejected.
///
/// This test requires setting the TEST_FAKE_ACTIVATION_HEIGHTS.
#[test]
#[allow(clippy::print_stderr)]
fn all_upgrades_and_wrong_commitments_with_fake_activation_heights() -> Result<()> {
    let _init_guard = zebra_test::init();

    if std::env::var_os("TEST_FAKE_ACTIVATION_HEIGHTS").is_none() {
        eprintln!("Skipping all_upgrades_and_wrong_commitments_with_fake_activation_heights() since $TEST_FAKE_ACTIVATION_HEIGHTS is NOT set");
        return Ok(());
    }

    // Use no_shrink() because we're ignoring _count and there is nothing to actually shrink.
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_valid_commitments().no_shrink())| {

            let mut state = FinalizedState::new(&Config::ephemeral(), network);
            let mut height = Height(0);
            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(network).unwrap();
            let heartwood_height_plus1 = (heartwood_height + 1).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(network).unwrap();
            let nu5_height_plus1 = (nu5_height + 1).unwrap();

            let mut failure_count = 0;
            for block in chain.iter() {
                let block_hash = block.hash;
                let current_height = block.block.coinbase_height().unwrap();
                // For some specific heights, try to commit a block with
                // corrupted commitment.
                match current_height {
                    h if h == heartwood_height ||
                        h == heartwood_height_plus1 ||
                        h == nu5_height ||
                        h == nu5_height_plus1 => {
                            let block = block.block.clone().set_block_commitment([0x42; 32]);
                            state.commit_finalized_direct(
                                FinalizedBlock::from(block),
                                "all_upgrades test"
                            ).expect_err("Must fail commitment check");
                            failure_count += 1;
                        },
                    _ => {},
                }
                let hash = state.commit_finalized_direct(
                    FinalizedBlock::from(block.block.clone()),
                    "all_upgrades test"
                ).unwrap();
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                prop_assert_eq!(hash, block_hash);
                height = Height(height.0 + 1);
            }
            // Make sure the failure path was triggered
            prop_assert_eq!(failure_count, 4);
    });

    Ok(())
}
