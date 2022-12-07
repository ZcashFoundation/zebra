//! Reading address transaction IDs.
//!
//! In the functions in this module:
//!
//! The block write task commits blocks to the finalized state before updating
//! `chain` with a cached copy of the best non-finalized chain from
//! `NonFinalizedState.chain_set`. Then the block commit task can commit additional blocks to
//! the finalized state after we've cloned the `chain`.
//!
//! This means that some blocks can be in both:
//! - the cached [`Chain`], and
//! - the shared finalized [`ZebraDb`] reference.

use std::{
    collections::{BTreeMap, HashSet},
    ops::RangeInclusive,
};

use zebra_chain::{block::Height, transaction, transparent};

use crate::{
    service::{
        finalized_state::ZebraDb, non_finalized_state::Chain, read::FINALIZED_STATE_QUERY_RETRIES,
    },
    BoxError, TransactionLocation,
};

/// Returns the transaction IDs that sent or received funds from the supplied [`transparent::Address`]es,
/// within `query_height_range`, in chain order.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// or the `query_height_range` is totally outside both the `chain` and `db` range,
/// returns an empty list.
pub fn transparent_tx_ids<C>(
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
    query_height_range: RangeInclusive<Height>,
) -> Result<BTreeMap<TransactionLocation, transaction::Hash>, BoxError>
where
    C: AsRef<Chain>,
{
    let mut tx_id_error = None;

    // Retry the finalized tx ID query if it was interrupted by a finalizing block,
    // and the non-finalized chain doesn't overlap the changed heights.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..=FINALIZED_STATE_QUERY_RETRIES {
        let (finalized_tx_ids, finalized_tip_range) =
            finalized_transparent_tx_ids(db, &addresses, query_height_range.clone());

        // Apply the non-finalized tx ID changes.
        let chain_tx_id_changes = chain_transparent_tx_id_changes(
            chain.as_ref(),
            &addresses,
            finalized_tip_range,
            query_height_range.clone(),
        );

        // If the tx IDs are valid, return them, otherwise, retry or return an error.
        match chain_tx_id_changes {
            Ok(chain_tx_id_changes) => {
                let tx_ids = apply_tx_id_changes(finalized_tx_ids, chain_tx_id_changes);

                return Ok(tx_ids);
            }

            Err(error) => tx_id_error = Some(Err(error)),
        }
    }

    tx_id_error.expect("unexpected missing error: attempts should set error or return")
}

/// Returns the [`transaction::Hash`]es for `addresses` in the finalized chain `query_height_range`,
/// and the finalized tip heights the transaction IDs were queried at.
///
/// If the addresses do not exist in the finalized `db`, returns an empty list.
//
// TODO: turn the return type into a struct?
fn finalized_transparent_tx_ids(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
    query_height_range: RangeInclusive<Height>,
) -> (
    BTreeMap<TransactionLocation, transaction::Hash>,
    Option<RangeInclusive<Height>>,
) {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying transaction IDs.

    // Check if the finalized state changed while we were querying it
    let start_finalized_tip = db.finalized_tip_height();

    let finalized_tx_ids = db.partial_finalized_transparent_tx_ids(addresses, query_height_range);

    let end_finalized_tip = db.finalized_tip_height();

    let finalized_tip_range = if let (Some(start_finalized_tip), Some(end_finalized_tip)) =
        (start_finalized_tip, end_finalized_tip)
    {
        Some(start_finalized_tip..=end_finalized_tip)
    } else {
        // State is empty
        None
    };

    (finalized_tx_ids, finalized_tip_range)
}

/// Returns the extra transaction IDs for `addresses` in the non-finalized chain `query_height_range`,
/// matching or overlapping the transaction IDs for the `finalized_tip_range`,
///
/// If the addresses do not exist in the non-finalized `chain`, returns an empty list.
//
// TODO: turn the return type into a struct?
fn chain_transparent_tx_id_changes<C>(
    chain: Option<C>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip_range: Option<RangeInclusive<Height>>,
    query_height_range: RangeInclusive<Height>,
) -> Result<BTreeMap<TransactionLocation, transaction::Hash>, BoxError>
where
    C: AsRef<Chain>,
{
    let address_count = addresses.len();

    let finalized_tip_range = match finalized_tip_range {
        Some(finalized_tip_range) => finalized_tip_range,
        None => {
            assert!(
                chain.is_none(),
                "unexpected non-finalized chain when finalized state is empty"
            );

            debug!(
                ?finalized_tip_range,
                ?address_count,
                "chain address tx ID query: state is empty, no tx IDs available",
            );

            return Ok(Default::default());
        }
    };

    // # Correctness
    //
    // We can compensate for addresses with mismatching blocks,
    // by adding the overlapping non-finalized transaction IDs.
    //
    // If there is only one address, mismatches aren't possible,
    // because tx IDs are added to the finalized state in chain order (and never removed),
    // and they are queried in chain order.

    // Check if the finalized and non-finalized states match or overlap
    let required_min_non_finalized_root = finalized_tip_range.start().0 + 1;

    // Work out if we need to compensate for finalized query results from multiple heights:
    // - Ok contains the finalized tip height (no need to compensate)
    // - Err contains the required non-finalized chain overlap
    let finalized_tip_status = required_min_non_finalized_root..=finalized_tip_range.end().0;
    let finalized_tip_status = if finalized_tip_status.is_empty() {
        let finalized_tip_height = *finalized_tip_range.end();
        Ok(finalized_tip_height)
    } else {
        let required_non_finalized_overlap = finalized_tip_status;
        Err(required_non_finalized_overlap)
    };

    if chain.is_none() {
        if address_count <= 1 || finalized_tip_status.is_ok() {
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?finalized_tip_range,
                ?address_count,
                "chain address tx ID query: \
                 finalized chain is consistent, and non-finalized chain is empty",
            );

            return Ok(Default::default());
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is empty.
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?finalized_tip_range,
                ?address_count,
                "chain address tx ID query: \
                 finalized tip query was inconsistent, but non-finalized chain is empty",
            );

            return Err("unable to get tx IDs: \
                        state was committing a block, and non-finalized chain is empty"
                .into());
        }
    }

    let chain = chain.unwrap();
    let chain = chain.as_ref();

    let non_finalized_root = chain.non_finalized_root_height();
    let non_finalized_tip = chain.non_finalized_tip_height();

    assert!(
        non_finalized_root.0 <= required_min_non_finalized_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized",
    );

    match finalized_tip_status {
        Ok(finalized_tip_height) => {
            // If we've already committed this entire chain, ignore its UTXO changes.
            // This is more likely if the non-finalized state is just getting started.
            if finalized_tip_height >= non_finalized_tip {
                debug!(
                    ?non_finalized_root,
                    ?non_finalized_tip,
                    ?finalized_tip_status,
                    ?finalized_tip_range,
                    ?address_count,
                    "chain address tx ID query: \
                     non-finalized blocks have all been finalized, no new UTXO changes",
                );

                return Ok(Default::default());
            }
        }

        Err(ref required_non_finalized_overlap) => {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is below the inconsistent query range.
            if address_count > 1 && *required_non_finalized_overlap.end() > non_finalized_tip.0 {
                debug!(
                    ?non_finalized_root,
                    ?non_finalized_tip,
                    ?finalized_tip_status,
                    ?finalized_tip_range,
                    ?address_count,
                    "chain address tx ID query: \
                     finalized tip query was inconsistent, \
                     some inconsistent blocks are missing from the non-finalized chain, \
                     and the query has multiple addresses",
                );

                return Err("unable to get tx IDs: \
                            state was committing a block, \
                            that is missing from the non-finalized chain, \
                            and the query has multiple addresses"
                    .into());
            }

            // Correctness: some finalized UTXOs might have duplicate creates or spends,
            // but we've just checked they can be corrected by applying the non-finalized UTXO changes.
            assert!(
                address_count <= 1
                    || required_non_finalized_overlap
                        .clone()
                        .all(|height| chain.blocks.contains_key(&Height(height))),
                "tx ID query inconsistency: \
                 chain must contain required overlap blocks \
                 or query must only have one address",
            );
        }
    }

    Ok(chain.partial_transparent_tx_ids(addresses, query_height_range))
}

/// Returns the combined finalized and non-finalized transaction IDs.
fn apply_tx_id_changes(
    finalized_tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
    chain_tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
) -> BTreeMap<TransactionLocation, transaction::Hash> {
    // Correctness: compensate for inconsistent tx IDs finalized blocks across multiple addresses,
    // by combining them with overlapping non-finalized block tx IDs.
    finalized_tx_ids
        .into_iter()
        .chain(chain_tx_ids.into_iter())
        .collect()
}
