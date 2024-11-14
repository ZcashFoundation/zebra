//! Checks for issuance and burn validity.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::orchard_zsa::IssuedAssets;

use crate::{SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    let Some(issued_assets_change) = semantically_verified.issued_assets_change.clone() else {
        return Ok(IssuedAssets::default());
    };

    let mut issued_assets = HashMap::new();

    for (asset_base, change) in issued_assets_change {
        let asset_state = issued_assets
            .get(&asset_base)
            .copied()
            .or_else(|| parent_chain.issued_asset(&asset_base))
            .or_else(|| finalized_state.issued_asset(&asset_base));

        let updated_asset_state = if change.is_burn() {
            asset_state
                .ok_or(ValidateContextError::InvalidBurn)?
                .apply_change(change)
                .ok_or(ValidateContextError::InvalidBurn)?
        } else {
            asset_state
                .unwrap_or_default()
                .apply_change(change)
                .ok_or(ValidateContextError::InvalidIssuance)?
        };

        issued_assets
            .insert(asset_base, updated_asset_state)
            .expect("transactions must have only one burn item per asset base");
    }

    Ok(issued_assets.into())
}
