//! Checks for issuance and burn validity.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::orchard_zsa::{AssetBase, AssetState, IssuedAssets, IssuedAssetsChange};

use crate::{service::read, SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    let mut issued_assets = HashMap::new();

    // Burns need to be checked and asset state changes need to be applied per tranaction, in case
    // the asset being burned was also issued in an earlier transaction in the same block.
    for transaction in &semantically_verified.block.transactions {
        let issued_assets_change = IssuedAssetsChange::from_transaction(transaction)
            .ok_or(ValidateContextError::InvalidIssuance)?;

        // Check that no burn item attempts to burn more than the issued supply for an asset
        for burn in transaction.orchard_burns() {
            let asset_base = burn.asset();
            let asset_state =
                asset_state(finalized_state, parent_chain, &issued_assets, &asset_base)
                    // The asset being burned should have been issued by a previous transaction, and
                    // any assets issued in previous transactions should be present in the issued assets map.
                    .ok_or(ValidateContextError::InvalidBurn)?;

            if asset_state.total_supply < burn.raw_amount() {
                return Err(ValidateContextError::InvalidBurn);
            } else {
                // Any burned asset bases in the transaction will also be present in the issued assets change,
                // adding a copy of initial asset state to `issued_assets` avoids duplicate disk reads.
                issued_assets.insert(asset_base, asset_state);
            }
        }

        // TODO: Remove the `issued_assets_change` field from `SemanticallyVerifiedBlock` and get the changes
        //       directly from transactions here and when writing blocks to disk.
        for (asset_base, change) in issued_assets_change.iter() {
            let asset_state =
                asset_state(finalized_state, parent_chain, &issued_assets, &asset_base)
                    .unwrap_or_default();

            let updated_asset_state = asset_state
                .apply_change(change)
                .ok_or(ValidateContextError::InvalidIssuance)?;

            // TODO: Update `Burn` to `HashMap<AssetBase, NoteValue>)` and return an error during deserialization if
            //       any asset base is burned twice in the same transaction
            issued_assets.insert(asset_base, updated_asset_state);
        }
    }

    Ok(issued_assets.into())
}

fn asset_state(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    issued_assets: &HashMap<AssetBase, AssetState>,
    asset_base: &AssetBase,
) -> Option<AssetState> {
    issued_assets
        .get(asset_base)
        .copied()
        .or_else(|| read::asset_state(Some(parent_chain), finalized_state, asset_base))
}
