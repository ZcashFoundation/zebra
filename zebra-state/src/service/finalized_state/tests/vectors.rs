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

use self::assert_eq;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    zebra_test::init();

    let genesis_block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let test_block: Arc<Block> = zebra_test::vectors::GENERATED_V5_1.zcash_deserialize_into()?;

    let mut state = FinalizedState::new(&Config::ephemeral(), Network::Mainnet);

    let hash = state
        .commit_finalized_direct(FinalizedBlock::from(genesis_block.clone()))
        .unwrap();
    assert_eq!(Some(Height(0)), state.finalized_tip_height());
    assert_eq!(hash, genesis_block.hash());

    let hash = state
        .commit_finalized_direct(FinalizedBlock::from(test_block.clone()))
        .unwrap();
    assert_eq!(Some(Height(1)), state.finalized_tip_height());
    assert_eq!(hash, test_block.hash());

    Ok(())
}
