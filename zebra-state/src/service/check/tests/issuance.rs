use std::sync::Arc;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block},
    orchard_zsa::IssuedAssets,
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_test::vectors::{OrchardWorkflowBlock, ORCHARD_ZSA_WORKFLOW_BLOCKS};

use crate::{
    check::{self, Chain},
    service::{finalized_state::FinalizedState, write::validate_and_commit_non_finalized},
    CheckpointVerifiedBlock, Config, NonFinalizedState,
};

fn valid_issuance_blocks() -> Vec<Arc<Block>> {
    ORCHARD_ZSA_WORKFLOW_BLOCKS
        .iter()
        .map(|OrchardWorkflowBlock { bytes, .. }| {
            Arc::new(Block::zcash_deserialize(&bytes[..]).expect("block should deserialize"))
        })
        .collect()
}

#[test]
fn check_burns_and_issuance() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Some(1), None, Some(1));

    let mut finalized_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    let mut non_finalized_state = NonFinalizedState::new(&network);

    let regtest_genesis_block = regtest_genesis_block();
    let regtest_genesis_hash = regtest_genesis_block.hash();

    finalized_state
        .commit_finalized_direct(regtest_genesis_block.into(), None, "test")
        .expect("unexpected invalid genesis block test vector");

    let block = valid_issuance_blocks().first().unwrap().clone();
    let mut header = Arc::<block::Header>::unwrap_or_clone(block.header.clone());
    header.previous_block_hash = regtest_genesis_hash;
    header.commitment_bytes = [0; 32].into();
    let block = Arc::new(Block {
        header: Arc::new(header),
        transactions: block.transactions.clone(),
    });

    let CheckpointVerifiedBlock(block) = CheckpointVerifiedBlock::new(block, None, None);

    let empty_chain = Chain::new(
        &network,
        finalized_state
            .db
            .finalized_tip_height()
            .unwrap_or(block::Height::MIN),
        finalized_state.db.sprout_tree_for_tip(),
        finalized_state.db.sapling_tree_for_tip(),
        finalized_state.db.orchard_tree_for_tip(),
        finalized_state.db.history_tree(),
        finalized_state.db.finalized_value_pool(),
    );

    let block_1_issued_assets = check::issuance::valid_burns_and_issuance(
        &finalized_state.db,
        &Arc::new(empty_chain),
        &block,
    )
    .expect("test transactions should be valid");

    validate_and_commit_non_finalized(&finalized_state.db, &mut non_finalized_state, block)
        .expect("validation should succeed");

    let best_chain = non_finalized_state
        .best_chain()
        .expect("should have a non-finalized chain");

    assert_eq!(
        IssuedAssets::from(best_chain.issued_assets.clone()),
        block_1_issued_assets,
        "issued assets for chain should match those of block 1"
    );
}
