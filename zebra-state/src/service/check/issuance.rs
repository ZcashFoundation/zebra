use std::{collections::HashMap, sync::Arc};

use zebra_chain::{orchard::AssetBase, transaction::Transaction};

use crate::{service::finalized_state::AssetState, ValidateContextError};

#[tracing::instrument(skip(issued_assets, transaction, modifier))]
fn modify_non_finalized_chain<F: Fn(&mut AssetState, AssetState)>(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
    modifier: F,
) {
    for (asset_base, change) in AssetState::from_transaction(transaction) {
        issued_assets
            .entry(asset_base)
            .and_modify(|prev| modifier(prev, change))
            .or_insert(change);
    }
}

#[tracing::instrument(skip(issued_assets, transaction))]
pub(crate) fn add_to_non_finalized_chain(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
) -> Result<(), ValidateContextError> {
    modify_non_finalized_chain(issued_assets, transaction, |prev, change| {
        prev.apply_change(change);
    });

    Ok(())
}

#[tracing::instrument(skip(issued_assets, transaction))]
pub(crate) fn remove_from_non_finalized_chain(
    issued_assets: &mut HashMap<AssetBase, AssetState>,
    transaction: &Arc<Transaction>,
) {
    modify_non_finalized_chain(issued_assets, transaction, |prev, change| {
        prev.revert_change(change);
    });
}
