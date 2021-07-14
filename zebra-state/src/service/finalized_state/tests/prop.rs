use std::env;

use zebra_chain::block::Height;
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::{
        arbitrary::PreparedChain,
        finalized_state::{FinalizedBlock, FinalizedState},
    },
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 32;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    zebra_test::init();
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network) in PreparedChain::default())| {
            let mut state = FinalizedState::new(&Config::ephemeral(), network);
            let mut height = Height(0);
            // use `count` to minimize test failures, so they are easier to diagnose
            for block in chain.iter().take(count) {
                let hash = state.commit_finalized_direct(
                    FinalizedBlock::from(block.clone()),
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
