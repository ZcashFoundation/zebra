use std::sync::Arc;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block},
    orchard_zsa::{AssetBase, IssuedAssetChanges},
    parameters::{testnet::ConfiguredActivationHeights, Network},
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

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu7: Some(1),
            ..Default::default()
        }
        .into(),
    );

    let mut finalized_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    let mut non_finalized_state = NonFinalizedState::new(&network);

    finalized_state
        .commit_finalized_direct(regtest_genesis_block().into(), None, "test")
        .expect("unexpected invalid genesis block test vector");

    let empty_chain = Arc::new(Chain::new(
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
    ));

    for OrchardWorkflowBlock {
        height,
        bytes,
        is_valid,
    } in ORCHARD_ZSA_WORKFLOW_BLOCKS.iter()
    {
        let block =
            Arc::new(Block::zcash_deserialize(&bytes[..]).expect("block should deserialize"));

        let chain = non_finalized_state
            .best_chain()
            .cloned()
            .unwrap_or_else(|| empty_chain.clone());

        let issued_asset_changes_result = IssuedAssetChanges::validate_and_get_changes(
            &block.transactions,
            None,
            |asset_base: &AssetBase| {
                read::asset_state(Some(&chain), &finalized_state.db, asset_base)
            },
        );

        let CheckpointVerifiedBlock(block) = CheckpointVerifiedBlock::new(block, None, None);

        let commit_result =
            validate_and_commit_non_finalized(&finalized_state.db, &mut non_finalized_state, block);

        if !is_valid {
            assert!(
                issued_asset_changes_result.is_err() || commit_result.is_err(),
                "invalid workflow block at height {height} should fail issued-asset validation or commit"
            );

            // Later workflow blocks depend on this one, so stop after rejecting it.
            break;
        }

        issued_asset_changes_result.unwrap_or_else(|error| {
            panic!(
                "valid workflow block at height {height} should have valid issued-asset changes: {error:?}"
            )
        });

        commit_result.unwrap_or_else(|error| {
            panic!("valid workflow block at height {height} should commit: {error:?}")
        });

        let best_chain = non_finalized_state
            .best_chain()
            .expect("should have a non-finalized chain");

        assert!(
            !best_chain.issued_assets.is_empty(),
            "issued assets should not be empty after workflow block {height}"
        );
    }
}
