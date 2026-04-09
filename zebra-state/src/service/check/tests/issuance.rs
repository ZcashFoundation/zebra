use std::sync::Arc;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block},
    orchard_zsa::{AssetBase, IssuedAssetChanges},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_test::vectors::{OrchardWorkflowBlock, ORCHARD_ZSA_WORKFLOW_BLOCKS};

use crate::{
    check::Chain,
    service::{finalized_state::FinalizedState, read, write::validate_and_commit_non_finalized},
    CheckpointVerifiedBlock, Config, NonFinalizedState,
};

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

    let mut block_iter =
        ORCHARD_ZSA_WORKFLOW_BLOCKS
            .iter()
            .map(|OrchardWorkflowBlock { bytes, .. }| {
                Arc::new(Block::zcash_deserialize(&bytes[..]).expect("block should deserialize"))
            });

    let genesis_block = regtest_genesis_block();

    finalized_state
        .commit_finalized_direct(genesis_block.into(), None, "test")
        .expect("unexpected invalid genesis block test vector");

    let block_1 = {
        let mut block = (*block_iter.next().expect("block 1 must exist")).clone();
        Arc::make_mut(&mut block.header).commitment_bytes = [0; 32].into();
        Arc::new(block)
    };

    let CheckpointVerifiedBlock(block_1) = CheckpointVerifiedBlock::new(block_1, None, None);

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

    let block_1_issued_assets = IssuedAssetChanges::validate_and_get_changes(
        &block_1.block.transactions,
        None,
        |asset_base: &AssetBase| {
            read::asset_state(
                Some(&Arc::new(empty_chain.clone())),
                &finalized_state.db,
                asset_base,
            )
        },
    )
    .expect("test transactions should be valid");

    validate_and_commit_non_finalized(&finalized_state.db, &mut non_finalized_state, block_1)
        .expect("validation should succeed");

    let best_chain = non_finalized_state
        .best_chain()
        .expect("should have a non-finalized chain");

    assert_eq!(
        IssuedAssetChanges::from(best_chain.issued_assets.clone()),
        block_1_issued_assets,
        "issued assets for chain should match those of block 1"
    );
}
