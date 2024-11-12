//! Checks for issuance and burn validity.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::orchard_zsa::IssuedAssets;

use crate::{IssuedAssetsOrChanges, SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    let IssuedAssetsOrChanges::BurnAndIssuanceChanges { burns, issuance } =
        semantically_verified.issued_assets_changes.clone()
    else {
        panic!("unexpected variant in semantically verified block")
    };

    let mut issued_assets = HashMap::new();

    for (asset_base, burn_change) in burns.clone() {
        // TODO: Move this to a read fn.
        let updated_asset_state = parent_chain
            .issued_asset(&asset_base)
            .or_else(|| finalized_state.issued_asset(&asset_base))
            .ok_or(ValidateContextError::InvalidBurn)?
            .apply_change(burn_change)
            .ok_or(ValidateContextError::InvalidBurn)?;

        issued_assets
            .insert(asset_base, updated_asset_state)
            .expect("transactions must have only one burn item per asset base");
    }

    for (asset_base, issuance_change) in issuance.clone() {
        // TODO: Move this to a read fn.
        let Some(asset_state) = issued_assets
            .get(&asset_base)
            .copied()
            .or_else(|| parent_chain.issued_asset(&asset_base))
            .or_else(|| finalized_state.issued_asset(&asset_base))
        else {
            continue;
        };

        let _ = issued_assets.insert(
            asset_base,
            asset_state
                .apply_change(issuance_change)
                .ok_or(ValidateContextError::InvalidIssuance)?,
        );
    }

    Ok(issued_assets.into())
}
