use std::env;

use zebra_chain::{
    block::{Block, Height, LedgerState},
    parameters::Network,
};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::finalized_state::{FinalizedBlock, FinalizedState},
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 2;
const PARTIAL_CHAIN_SIZE: usize = 200;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    zebra_test::init();

    blocks_with_v5_transactions_for_network(Network::Mainnet)?;
    blocks_with_v5_transactions_for_network(Network::Testnet)?;

    Ok(())
}

fn blocks_with_v5_transactions_for_network(network: Network) -> Result<()> {
    let strategy = LedgerState::genesis_strategy(None)
        .prop_flat_map(|init| Block::partial_chain_strategy(init, PARTIAL_CHAIN_SIZE));

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |(chain in strategy)| {

            let mut state = FinalizedState::new(&Config::ephemeral(), network);

            let mut height = Height(0);
            for block in chain {
                let hash = state.commit_finalized_direct(FinalizedBlock::from(block.clone()));
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                prop_assert_eq!(hash.unwrap(), block.hash());

                height = Height(height.0 + 1);
            }
    });

    Ok(())
}
