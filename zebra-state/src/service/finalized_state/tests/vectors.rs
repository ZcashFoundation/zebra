use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::finalized_state::{FinalizedBlock, FinalizedState},
};

use crate::service::non_finalized_state::arbitrary::PreparedChain;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    zebra_test::init();

    // commit only 1 random block after genesis
    const JUST_ONE: u32 = 1;

    // get the genesis block
    let genesis_block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;

    proptest!(ProptestConfig::with_cases(JUST_ONE), |((chain, _) in PreparedChain::default())| {
        // create a state
        let mut state = FinalizedState::new(&Config::ephemeral(), Network::Mainnet);

        // commit the genesis block
        let hash = state
            .commit_finalized_direct(FinalizedBlock::from(genesis_block.clone()))
            .unwrap();
        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert_eq!(hash, genesis_block.hash());

        // commit the generated block right after genesis
        let block = &chain.first().unwrap().block;
        let hash = state.commit_finalized_direct(FinalizedBlock::from(block.clone()));
        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert_eq!(hash.unwrap(), block.hash());
    });

    Ok(())
}
