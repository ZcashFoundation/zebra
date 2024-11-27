//! Checks for issuance and burn validity.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::orchard_zsa::{AssetBase, AssetState, IssuedAssets};

use crate::{SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

// TODO: Factor out chain/disk read to a fn in the `read` module.
fn asset_state(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    issued_assets: &HashMap<AssetBase, AssetState>,
    asset_base: &AssetBase,
) -> Option<AssetState> {
    issued_assets
        .get(asset_base)
        .copied()
        .or_else(|| parent_chain.issued_asset(asset_base))
        .or_else(|| finalized_state.issued_asset(asset_base))
}

pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    let mut issued_assets = HashMap::new();

    for (issued_assets_change, transaction) in semantically_verified
        .issued_assets_changes
        .iter()
        .zip(&semantically_verified.block.transactions)
    {
        // Check that no burn item attempts to burn more than the issued supply for an asset
        for burn in transaction.orchard_burns() {
            let asset_base = burn.asset();
            let asset_state =
                asset_state(finalized_state, parent_chain, &issued_assets, &asset_base)
                    .ok_or(ValidateContextError::InvalidBurn)?;

            if asset_state.total_supply < burn.amount() {
                return Err(ValidateContextError::InvalidBurn);
            } else {
                // Any burned asset bases in the transaction will also be present in the issued assets change,
                // adding a copy of initial asset state to `issued_assets` avoids duplicate disk reads.
                issued_assets.insert(asset_base, asset_state);
            }
        }

        for (asset_base, change) in issued_assets_change.iter() {
            let asset_state =
                asset_state(finalized_state, parent_chain, &issued_assets, &asset_base)
                    .unwrap_or_default();

            let updated_asset_state = asset_state
                .apply_change(change)
                .ok_or(ValidateContextError::InvalidIssuance)?;

            issued_assets
                .insert(asset_base, updated_asset_state)
                .expect("transactions must have only one burn item per asset base");
        }
    }

    Ok(issued_assets.into())
}
