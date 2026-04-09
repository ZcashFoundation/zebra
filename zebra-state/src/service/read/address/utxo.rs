//! Transparent address index UTXO queries.
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
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::RangeInclusive,
};

use derive_getters::Getters;
use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    transaction, transparent,
};

use crate::{
    service::{
        finalized_state::ZebraDb, non_finalized_state::Chain, read::FINALIZED_STATE_QUERY_RETRIES,
    },
    BoxError, OutputLocation, TransactionLocation,
};

/// The full range of address heights.
///
/// The genesis coinbase transactions are ignored by a consensus rule,
/// so they are not included in any address indexes.
pub const ADDRESS_HEIGHTS_FULL_RANGE: RangeInclusive<Height> = Height(1)..=Height::MAX;

/// A convenience wrapper that efficiently stores unspent transparent outputs,
/// and the corresponding transaction IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Getters)]
pub struct AddressUtxos {
    /// A set of unspent transparent outputs.
    #[getter(skip)]
    utxos: BTreeMap<OutputLocation, transparent::Output>,

    /// The transaction IDs for each [`OutputLocation`] in `utxos`.
    #[getter(skip)]
    tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,

    /// The configured network for this state.
    #[getter(skip)]
    network: Network,

    /// The last height and hash that was queried to produce these UTXOs, if any.
    /// It will be None if the state is empty.
    last_height_and_hash: Option<(block::Height, block::Hash)>,
}

impl AddressUtxos {
    /// Creates a new set of address UTXOs.
    pub fn new(
        network: &Network,
        utxos: BTreeMap<OutputLocation, transparent::Output>,
        tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
        last_height_and_hash: Option<(block::Height, block::Hash)>,
    ) -> Self {
        Self {
            utxos,
            tx_ids,
            network: network.clone(),
            last_height_and_hash,
        }
    }

    /// Returns an iterator that provides the unspent output, its transaction hash,
    /// its location in the chain, and the address it was sent to.
    ///
    /// The UTXOs are returned in chain order, across all addresses.
    #[allow(dead_code)]
    pub fn utxos(
        &self,
    ) -> impl Iterator<
        Item = (
            transparent::Address,
            &transaction::Hash,
            &OutputLocation,
            &transparent::Output,
        ),
    > {
        self.utxos.iter().map(|(out_loc, output)| {
            (
                output
                    .address(&self.network)
                    .expect("address indexes only contain outputs with addresses"),
                self.tx_ids
                    .get(&out_loc.transaction_location())
                    .expect("address indexes are consistent"),
                out_loc,
                output,
            )
        })
    }
}

/// Returns the unspent transparent outputs (UTXOs) for the supplied [`transparent::Address`]es,
/// in chain order; and the transaction IDs for the transactions containing those UTXOs.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// returns an empty list.
pub fn address_utxos<C>(
    network: &Network,
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<AddressUtxos, BoxError>
where
    C: AsRef<Chain>,
{
    let mut utxo_error = None;
    let address_count = addresses.len();

    // Retry the finalized UTXO query if it was interrupted by a finalizing block,
    // and the non-finalized chain doesn't overlap the changed heights.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for attempt in 0..=FINALIZED_STATE_QUERY_RETRIES {
        debug!(?attempt, ?address_count, "starting address UTXO query");

        let (finalized_utxos, finalized_tip_range) = finalized_address_utxos(db, &addresses);

        debug!(
            finalized_utxo_count = ?finalized_utxos.len(),
            ?finalized_tip_range,
            ?address_count,
            ?attempt,
            "finalized address UTXO response",
        );

        // Apply the non-finalized UTXO changes.
        let chain_utxo_changes =
            chain_transparent_utxo_changes(chain.as_ref(), &addresses, finalized_tip_range);

        // If the UTXOs are valid, return them, otherwise, retry or return an error.
        match chain_utxo_changes {
            Ok((created_chain_utxos, spent_chain_utxos, last_height)) => {
                debug!(
                    chain_utxo_count = ?created_chain_utxos.len(),
                    chain_utxo_spent = ?spent_chain_utxos.len(),
                    ?address_count,
                    ?attempt,
                    "chain address UTXO response",
                );

                let utxos =
                    apply_utxo_changes(finalized_utxos, created_chain_utxos, spent_chain_utxos);
                let tx_ids = lookup_tx_ids_for_utxos(chain.as_ref(), db, &addresses, &utxos);

                debug!(
                    full_utxo_count = ?utxos.len(),
                    tx_id_count = ?tx_ids.len(),
                    ?address_count,
                    ?attempt,
                    "full address UTXO response",
                );

                // Get the matching hash for the given height, if any
                let last_height_and_hash = last_height.and_then(|height| {
                    chain
                        .as_ref()
                        .and_then(|c| c.as_ref().hash_by_height(height))
                        .or_else(|| db.hash(height))
                        .map(|hash| (height, hash))
                });

                return Ok(AddressUtxos::new(
                    network,
                    utxos,
                    tx_ids,
                    last_height_and_hash,
                ));
            }

            Err(chain_utxo_error) => {
                debug!(
                    ?chain_utxo_error,
                    ?address_count,
                    ?attempt,
                    "chain address UTXO response",
                );

                utxo_error = Some(Err(chain_utxo_error))
            }
        }
    }

    utxo_error.expect("unexpected missing error: attempts should set error or return")
}

/// Returns the unspent transparent outputs (UTXOs) for `addresses` in the finalized chain,
/// and the finalized tip heights the UTXOs were queried at.
///
/// If the addresses do not exist in the finalized `db`, returns an empty list.
//
// TODO: turn the return type into a struct?
fn finalized_address_utxos(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
) -> (
    BTreeMap<OutputLocation, transparent::Output>,
    Option<RangeInclusive<Height>>,
) {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying address UTXOs.

    // Check if the finalized state changed while we were querying it
    let start_finalized_tip = db.finalized_tip_height();

    let finalized_utxos = db.partial_finalized_address_utxos(addresses);

    let end_finalized_tip = db.finalized_tip_height();

    let finalized_tip_range = if let (Some(start_finalized_tip), Some(end_finalized_tip)) =
        (start_finalized_tip, end_finalized_tip)
    {
        Some(start_finalized_tip..=end_finalized_tip)
    } else {
        // State is empty
        None
    };

    (finalized_utxos, finalized_tip_range)
}

/// Returns the UTXO changes (created and spent) for `addresses` in the
/// non-finalized chain, matching or overlapping the UTXOs for the
/// `finalized_tip_range`. Also returns the height of the last block in which
/// the changes were located, or None if the state is empty.
///
/// If the addresses do not exist in the non-finalized `chain`, returns an empty
/// list.
//
// TODO: turn the return type into a struct?
fn chain_transparent_utxo_changes<C>(
    chain: Option<C>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip_range: Option<RangeInclusive<Height>>,
) -> Result<
    (
        BTreeMap<OutputLocation, transparent::Output>,
        BTreeSet<OutputLocation>,
        Option<Height>,
    ),
    BoxError,
>
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
                "chain address UTXO query: state is empty, no UTXOs available",
            );

            return Ok(Default::default());
        }
    };

    // # Correctness
    //
    // We can compensate for deleted UTXOs by applying the overlapping non-finalized UTXO changes.

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
        if let Ok(finalized_tip_height) = finalized_tip_status {
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?finalized_tip_range,
                ?address_count,
                "chain address UTXO query: \
                 finalized chain is consistent, and non-finalized chain is empty",
            );

            return Ok((
                Default::default(),
                Default::default(),
                Some(finalized_tip_height),
            ));
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is empty.
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?finalized_tip_range,
                ?address_count,
                "chain address UTXO query: \
                 finalized tip query was inconsistent, but non-finalized chain is empty",
            );

            return Err("unable to get UTXOs: \
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
                    "chain address UTXO query: \
                     non-finalized blocks have all been finalized, no new UTXO changes",
                );

                return Ok((
                    Default::default(),
                    Default::default(),
                    Some(finalized_tip_height),
                ));
            }
        }

        Err(ref required_non_finalized_overlap) => {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is below the inconsistent query range.
            if *required_non_finalized_overlap.end() > non_finalized_tip.0 {
                debug!(
                    ?non_finalized_root,
                    ?non_finalized_tip,
                    ?finalized_tip_status,
                    ?finalized_tip_range,
                    ?address_count,
                    "chain address UTXO query: \
                     finalized tip query was inconsistent, \
                     and some inconsistent blocks are missing from the non-finalized chain",
                );

                return Err("unable to get UTXOs: \
                            state was committing a block, \
                            that is missing from the non-finalized chain"
                    .into());
            }

            // Correctness: some finalized UTXOs might have duplicate creates or spends,
            // but we've just checked they can be corrected by applying the non-finalized UTXO changes.
            assert!(
                required_non_finalized_overlap
                    .clone()
                    .all(|height| chain.blocks.contains_key(&Height(height))),
                "UTXO query inconsistency: chain must contain required overlap blocks",
            );
        }
    }
    let (created, spent) = chain.partial_transparent_utxo_changes(addresses);
    Ok((created, spent, Some(non_finalized_tip)))
}

/// Combines the supplied finalized and non-finalized UTXOs,
/// removes the spent UTXOs, and returns the result.
fn apply_utxo_changes(
    finalized_utxos: BTreeMap<OutputLocation, transparent::Output>,
    created_chain_utxos: BTreeMap<OutputLocation, transparent::Output>,
    spent_chain_utxos: BTreeSet<OutputLocation>,
) -> BTreeMap<OutputLocation, transparent::Output> {
    // Correctness: combine the created UTXOs, then remove spent UTXOs,
    // to compensate for overlapping finalized and non-finalized blocks.
    finalized_utxos
        .into_iter()
        .chain(created_chain_utxos)
        .filter(|(utxo_location, _output)| !spent_chain_utxos.contains(utxo_location))
        .collect()
}

/// Returns the [`transaction::Hash`]es containing the supplied UTXOs,
/// from the non-finalized `chain` and finalized `db`.
///
/// # Panics
///
/// If any UTXO is not in the supplied state.
fn lookup_tx_ids_for_utxos<C>(
    chain: Option<C>,
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
    utxos: &BTreeMap<OutputLocation, transparent::Output>,
) -> BTreeMap<TransactionLocation, transaction::Hash>
where
    C: AsRef<Chain>,
{
    // Get the unique set of transaction locations
    let transaction_locations: BTreeSet<TransactionLocation> = utxos
        .keys()
        .map(|output_location| output_location.transaction_location())
        .collect();

    let chain_tx_ids = chain
        .as_ref()
        .map(|chain| {
            chain
                .as_ref()
                .partial_transparent_tx_ids(addresses, ADDRESS_HEIGHTS_FULL_RANGE)
        })
        .unwrap_or_default();

    // First try the in-memory chain, then the disk database
    transaction_locations
        .iter()
        .map(|tx_loc| {
            (
                *tx_loc,
                chain_tx_ids.get(tx_loc).cloned().unwrap_or_else(|| {
                    db.transaction_hash(*tx_loc)
                        .expect("unexpected inconsistent UTXO indexes")
                }),
            )
        })
        .collect()
}
