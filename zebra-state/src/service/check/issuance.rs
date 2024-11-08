use std::{collections::HashMap, sync::Arc};

use zebra_chain::{orchard::AssetBase, transaction::Transaction};

use crate::{service::finalized_state::AssetState, ValidateContextError};

#[tracing::instrument(skip(issued_assets, transaction, modifier))]
fn modify_non_finalized_chain<F: Fn(AssetState, AssetState) -> AssetState>(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
    modifier: F,
) {
    for (asset_base, change) in AssetState::from_transaction(transaction) {
        let new_state = issued_assets
            .get(&asset_base)
            .map_or(change, |&prev| modifier(prev, change));
        issued_assets.insert(asset_base, new_state);
    }
}

#[tracing::instrument(skip(issued_assets, transaction))]
pub(crate) fn add_to_non_finalized_chain(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
) -> Result<(), ValidateContextError> {
    modify_non_finalized_chain(issued_assets, transaction, |prev, change| {
        prev.with_change(change)
    });

    Ok(())
}

#[tracing::instrument(skip(issued_assets, transaction))]
pub(crate) fn remove_from_non_finalized_chain(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
) {
    modify_non_finalized_chain(issued_assets, transaction, |prev, change| {
        prev.with_revert(change)
    });
}
