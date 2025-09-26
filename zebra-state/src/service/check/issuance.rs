//! Checks for issuance and burn validity.

use std::sync::Arc;

use zebra_chain::orchard_zsa::{AssetBase, AssetStateError, IssuedAssets};

use crate::{service::read, SemanticallyVerifiedBlock, ValidateContextError, ZebraDb};

use super::Chain;

// FIXME: consider removeing this function and calling IssuedAssets::validated_from_transactions
// directly (also imlpement From<AssetStateError> forValidateContextError)
pub fn valid_burns_and_issuance(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<IssuedAssets, ValidateContextError> {
    IssuedAssets::validated_from_transactions(
        &semantically_verified.block.transactions,
        |asset_base: &AssetBase| read::asset_state(Some(parent_chain), finalized_state, asset_base),
    )
    // FIXME: reuse orchard errors, implement From for the conversion etc.
    .map_err(|error| match error {
        AssetStateError::InvalidBurn => ValidateContextError::InvalidBurn,
        AssetStateError::InvalidIssuance => ValidateContextError::InvalidIssuance,
    })
}
