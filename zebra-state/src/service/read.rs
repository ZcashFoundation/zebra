//! Shared state reading code.
//!
//! Used by [`StateService`] and [`ReadStateService`]
//! to read from the best [`Chain`] in the [`NonFinalizedState`],
//! and the database in the [`FinalizedState`].

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::RangeInclusive,
    sync::Arc,
};

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    block::{self, Block, Height},
    parameters::Network,
    transaction::{self, Transaction},
    transparent,
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    BoxError, HashOrHeight, OutputLocation, TransactionLocation,
};

pub mod utxo;

#[cfg(test)]
mod tests;

pub use utxo::AddressUtxos;

/// If the transparent address index queries are interrupted by a new finalized block,
/// retry this many times.
///
/// Once we're at the tip, we expect up to 2 blocks to arrive at the same time.
/// If any more arrive, the client should wait until we're synchronised with our peers.
const FINALIZED_ADDRESS_INDEX_RETRIES: usize = 3;

/// The full range of address heights.
///
/// The genesis coinbase transactions are ignored by a consensus rule,
/// so they are not included in any address indexes.
pub const ADDRESS_HEIGHTS_FULL_RANGE: RangeInclusive<Height> = Height(1)..=Height::MAX;

/// Returns the [`Block`] with [`block::Hash`](zebra_chain::block::Hash) or
/// [`Height`](zebra_chain::block::Height),
/// if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<Block>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // Since blocks are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first.
    // (`chain` is always in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}

/// Returns the [`Transaction`] with [`transaction::Hash`],
/// if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn transaction<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<(Arc<Transaction>, block::Height)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // Since transactions are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first.
    // (`chain` is always in memory, but `db` stores transactions on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| {
            chain
                .as_ref()
                .transaction(hash)
                .map(|(tx, height)| (tx.clone(), height))
        })
        .or_else(|| db.transaction(hash))
}

/// Returns the total transparent balance for the supplied [`transparent::Address`]es.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`, returns zero.
#[allow(dead_code)]
pub(crate) fn transparent_balance(
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<Amount<NonNegative>, BoxError> {
    let mut balance_result = finalized_transparent_balance(db, &addresses);

    // Retry the finalized balance query if it was interrupted by a finalizing block
    for _ in 0..FINALIZED_ADDRESS_INDEX_RETRIES {
        if balance_result.is_ok() {
            break;
        }

        balance_result = finalized_transparent_balance(db, &addresses);
    }

    let (mut balance, finalized_tip) = balance_result?;

    // Apply the non-finalized balance changes
    if let Some(chain) = chain {
        let chain_balance_change =
            chain_transparent_balance_change(chain, &addresses, finalized_tip);

        balance = apply_balance_change(balance, chain_balance_change).expect(
            "unexpected amount overflow: value balances are valid, so partial sum should be valid",
        );
    }

    Ok(balance)
}

/// Returns the total transparent balance for `addresses` in the finalized chain,
/// and the finalized tip height the balances were queried at.
///
/// If the addresses do not exist in the finalized `db`, returns zero.
//
// TODO: turn the return type into a struct?
fn finalized_transparent_balance(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
) -> Result<(Amount<NonNegative>, Option<Height>), BoxError> {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying address balances.

    // Check if the finalized state changed while we were querying it
    let original_finalized_tip = db.tip();

    let finalized_balance = db.partial_finalized_transparent_balance(addresses);

    let finalized_tip = db.tip();

    if original_finalized_tip != finalized_tip {
        // Correctness: Some balances might be from before the block, and some after
        return Err("unable to get balance: state was committing a block".into());
    }

    let finalized_tip = finalized_tip.map(|(height, _hash)| height);

    Ok((finalized_balance, finalized_tip))
}

/// Returns the total transparent balance change for `addresses` in the non-finalized chain,
/// matching the balance for the `finalized_tip`.
///
/// If the addresses do not exist in the non-finalized `chain`, returns zero.
fn chain_transparent_balance_change(
    mut chain: Arc<Chain>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip: Option<Height>,
) -> Amount<NegativeAllowed> {
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.

    // Check if the finalized and non-finalized states match
    let required_chain_root = finalized_tip
        .map(|tip| (tip + 1).unwrap())
        .unwrap_or(Height(0));

    let chain = Arc::make_mut(&mut chain);

    assert!(
        chain.non_finalized_root_height() <= required_chain_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized"
    );

    let chain_tip = chain.non_finalized_tip_height();

    // If we've already committed this entire chain, ignore its balance changes.
    // This is more likely if the non-finalized state is just getting started.
    if chain_tip < required_chain_root {
        return Amount::zero();
    }

    // Correctness: some balances might have duplicate creates or spends,
    // so we pop root blocks from `chain` until the chain root is a child of the finalized tip.
    while chain.non_finalized_root_height() < required_chain_root {
        // TODO: just revert the transparent balances, to improve performance
        chain.pop_root();
    }

    chain.partial_transparent_balance_change(addresses)
}

/// Add the supplied finalized and non-finalized balances together,
/// and return the result.
fn apply_balance_change(
    finalized_balance: Amount<NonNegative>,
    chain_balance_change: Amount<NegativeAllowed>,
) -> amount::Result<Amount<NonNegative>> {
    let balance = finalized_balance.constrain()? + chain_balance_change;

    balance?.constrain()
}

/// Returns the unspent transparent outputs (UTXOs) for the supplied [`transparent::Address`]es,
/// in chain order; and the transaction IDs for the transactions containing those UTXOs.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// returns an empty list.
#[allow(dead_code)]
pub(crate) fn transparent_utxos<C>(
    network: Network,
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<AddressUtxos, BoxError>
where
    C: AsRef<Chain>,
{
    let mut utxo_error = None;

    // Retry the finalized UTXO query if it was interrupted by a finalizing block,
    // and the non-finalized chain doesn't overlap the changed heights.
    for _ in 0..=FINALIZED_ADDRESS_INDEX_RETRIES {
        let (finalized_utxos, finalized_tip_range) = finalized_transparent_utxos(db, &addresses);

        // Apply the non-finalized UTXO changes.
        let chain_utxo_changes =
            chain_transparent_utxo_changes(chain.as_ref(), &addresses, finalized_tip_range);

        // If the UTXOs are valid, return them, otherwise, retry or return an error.
        match chain_utxo_changes {
            Ok(chain_utxo_changes) => {
                let utxos = apply_utxo_changes(finalized_utxos, chain_utxo_changes);
                let tx_ids = lookup_tx_ids_for_utxos(chain, db, &addresses, &utxos);

                return Ok(AddressUtxos::new(network, utxos, tx_ids));
            }

            Err(error) => utxo_error = Some(Err(error)),
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
fn finalized_transparent_utxos(
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

    let finalized_utxos = db.partial_finalized_transparent_utxos(addresses);

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

/// Returns the UTXO changes for `addresses` in the non-finalized chain,
/// matching or overlapping the UTXOs for the `finalized_tip_range`.
///
/// If the addresses do not exist in the non-finalized `chain`, returns an empty list.
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
    ),
    BoxError,
>
where
    C: AsRef<Chain>,
{
    let finalized_tip_range = match finalized_tip_range {
        Some(finalized_tip_range) => finalized_tip_range,
        None => {
            assert!(
                chain.is_none(),
                "unexpected non-finalized chain when finalized state is empty"
            );

            // Empty chains don't contain any changes.
            return Ok(Default::default());
        }
    };

    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // But we can compensate for deleted UTXOs by applying the overlapping non-finalized UTXO changes.

    // Check if the finalized and non-finalized states match or overlap
    let required_min_chain_root = finalized_tip_range.start().0 + 1;
    let mut required_chain_overlap = required_min_chain_root..=finalized_tip_range.end().0;

    if chain.is_none() {
        if required_chain_overlap.is_empty() {
            // The non-finalized chain is empty, and we don't need it.
            return Ok(Default::default());
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is empty.
            return Err("unable to get UTXOs: state was committing a block, and non-finalized chain is empty".into());
        }
    }

    let chain = chain.unwrap();
    let chain = chain.as_ref();

    let chain_root = chain.non_finalized_root_height().0;
    let chain_tip = chain.non_finalized_tip_height().0;

    assert!(
        chain_root <= required_min_chain_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized"
    );

    // If we've already committed this entire chain, ignore its UTXO changes.
    // This is more likely if the non-finalized state is just getting started.
    if chain_tip > *required_chain_overlap.end() {
        if required_chain_overlap.is_empty() {
            // The non-finalized chain has been committed, and we don't need it.
            return Ok(Default::default());
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is below the inconsistent query range.
            return Err("unable to get UTXOs: state was committing a block, and non-finalized chain has been committed".into());
        }
    }

    // Correctness: some finalized UTXOs might have duplicate creates or spends,
    // but we've just checked they can be corrected by applying the non-finalized UTXO changes.
    assert!(
        required_chain_overlap.all(|height| chain.blocks.contains_key(&Height(height))),
        "UTXO query inconsistency: chain must contain required overlap blocks",
    );

    Ok(chain.partial_transparent_utxo_changes(addresses))
}

/// Combines the supplied finalized and non-finalized UTXOs,
/// removes the spent UTXOs, and returns the result.
fn apply_utxo_changes(
    finalized_utxos: BTreeMap<OutputLocation, transparent::Output>,
    (created_chain_utxos, spent_chain_utxos): (
        BTreeMap<OutputLocation, transparent::Output>,
        BTreeSet<OutputLocation>,
    ),
) -> BTreeMap<OutputLocation, transparent::Output> {
    // Correctness: combine the created UTXOs, then remove spent UTXOs,
    // to compensate for overlapping finalized and non-finalized blocks.
    finalized_utxos
        .into_iter()
        .chain(created_chain_utxos.into_iter())
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

/// Returns the transaction IDs that sent or received funds from the supplied [`transparent::Address`]es,
/// within `query_height_range`, in chain order.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// or the `query_height_range` is totally outside both the `chain` and `db` range,
/// returns an empty list.
pub(crate) fn transparent_tx_ids<C>(
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
    for _ in 0..=FINALIZED_ADDRESS_INDEX_RETRIES {
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
    let finalized_tip_range = match finalized_tip_range {
        Some(finalized_tip_range) => finalized_tip_range,
        None => {
            assert!(
                chain.is_none(),
                "unexpected non-finalized chain when finalized state is empty"
            );

            // Empty chains don't contain any tx IDs.
            return Ok(Default::default());
        }
    };

    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // But we can compensate for addresses with mismatching blocks,
    // by adding the overlapping non-finalized transaction IDs.
    //
    // If there is only one address, mismatches aren't possible,
    // because tx IDs are added to the finalized state in chain order (and never removed),
    // and they are queried in chain order.

    // Check if the finalized and non-finalized states match or overlap
    let required_min_chain_root = finalized_tip_range.start().0 + 1;
    let mut required_chain_overlap = required_min_chain_root..=finalized_tip_range.end().0;

    if chain.is_none() {
        if required_chain_overlap.is_empty() || addresses.len() <= 1 {
            // The non-finalized chain is empty, and we don't need it.
            return Ok(Default::default());
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is empty.
            return Err("unable to get tx IDs: state was committing a block, and non-finalized chain is empty".into());
        }
    }

    let chain = chain.unwrap();
    let chain = chain.as_ref();

    let chain_root = chain.non_finalized_root_height().0;
    let chain_tip = chain.non_finalized_tip_height().0;

    assert!(
        chain_root <= required_min_chain_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized"
    );

    // If we've already committed this entire chain, ignore its UTXO changes.
    // This is more likely if the non-finalized state is just getting started.
    if chain_tip > *required_chain_overlap.end() {
        if required_chain_overlap.is_empty() || addresses.len() <= 1 {
            // The non-finalized chain has been committed, and we don't need it.
            return Ok(Default::default());
        } else {
            // We can't compensate for inconsistent database queries,
            // because the non-finalized chain is below the inconsistent query range.
            return Err("unable to get tx IDs: state was committing a block, and non-finalized chain has been committed".into());
        }
    }

    // Correctness: some finalized tx IDs might have come from different blocks for different addresses,
    // but we've just checked they can be corrected by applying the non-finalized UTXO changes.
    assert!(
        required_chain_overlap.all(|height| chain.blocks.contains_key(&Height(height))) || addresses.len() <= 1,
        "tx ID query inconsistency: chain must contain required overlap blocks if there are multiple addresses",
    );

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
