//! Reading address indexes.

use std::sync::Arc;

use zebra_chain::parameters::Network;

use crate::{
    request::{AddressField, AddressQuery},
    response::QueriedAddresses,
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    BoxError,
};

pub mod balance;
pub mod tx_id;
pub mod utxo;

/// Returns the [`QueriedAddresses`] with the requested [`AddressField`]s for the
/// addresses in `query`.
///
/// Only the requested fields are read; fields that were not requested are never
/// computed.
pub fn address_query(
    network: &Network,
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    query: AddressQuery,
) -> Result<QueriedAddresses, BoxError> {
    // `Option<&Arc<Chain>>` is `Copy`, and `&Arc<Chain>: AsRef<Chain>` forwards
    // to `Arc<Chain>: AsRef<Chain>`, so this can be passed to the generic field
    // lookups below.
    let chain = chain.as_ref();
    let AddressQuery {
        addresses,
        height_range,
        fields,
    } = query;

    let mut queried_addresses = QueriedAddresses::default();

    if fields.contains(&AddressField::Balance) {
        let (balance, received) =
            balance::transparent_balance(chain.cloned(), db, addresses.clone())?;
        queried_addresses.balance = Some(balance);
        queried_addresses.received = Some(received);
    }

    if fields.contains(&AddressField::TransactionIds) {
        // Unlike the other fields, transaction IDs are always looked up in a
        // height range; default to the full chain when none was given.
        let height_range = height_range.unwrap_or(utxo::ADDRESS_HEIGHTS_FULL_RANGE);
        queried_addresses.transaction_ids = Some(tx_id::transparent_tx_ids(
            chain,
            db,
            addresses.clone(),
            height_range,
        )?);
    }

    if fields.contains(&AddressField::Utxos) {
        queried_addresses.utxos = Some(utxo::address_utxos(network, chain, db, addresses)?);
    }

    Ok(queried_addresses)
}
