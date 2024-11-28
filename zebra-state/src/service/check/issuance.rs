//! Checks for issuance and burn validity.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::orchard_zsa::{AssetBase, AssetState, IssuedAssets};

use crate::{service::read, SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    let mut issued_assets = HashMap::new();

    // FIXME: Do all checks (for duplication, existence etc.) need to be performed per tranaction, not per
    // the entire block?
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

            // FIXME: The check that we can't burn an asset before we issued it is implicit -
            // through the check it total_supply < burn.amount (the total supply is zero if the
            // asset is not issued). May be validation functions from the orcharfd crate need to be
            // reused in a some way?
            if asset_state.total_supply < burn.raw_amount() {
                return Err(ValidateContextError::InvalidBurn);
            } else {
                // Any burned asset bases in the transaction will also be present in the issued assets change,
                // adding a copy of initial asset state to `issued_assets` avoids duplicate disk reads.
                issued_assets.insert(asset_base, asset_state);
            }
        }

        // FIXME: Not sure: it looks like semantically_verified.issued_assets_changes is already
        // filled with burn and issuance items in zebra-consensus, see Verifier::call function in
        // zebra-consensus/src/transaction.rs (it uses from_burn and from_issue_action AssetStateChange
        // methods from ebra-chain/src/orchard_zsa/asset_state.rs). Can't it cause a duplication?
        // Can we collect all change items here, not in zebra-consensus (and so we don't need
        // SemanticallyVerifiedBlock::issued_assets_changes at all), or performing part of the
        // checks in zebra-consensus is important for the consensus checks order in a some way?
        for (asset_base, change) in issued_assets_change.iter() {
            let asset_state =
                asset_state(finalized_state, parent_chain, &issued_assets, &asset_base)
                    .unwrap_or_default();

            let updated_asset_state = asset_state
                .apply_change(change)
                .ok_or(ValidateContextError::InvalidIssuance)?;

            // FIXME: Is it correct to do nothing if the issued_assets aready has asset_base? Now it'd be
            // replaced with updated_asset_state in this case (where the duplicated value is added to
            // the supply). Block may have two burn records for the same asset but in different
            // transactions - it's allowed, that's why the check has been removed. On the other
            // hand, there needs to be a check that denies duplicated burn records for the same
            // asset inside the same transaction.
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
