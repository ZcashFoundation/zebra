//! Shared state reading code.
//!
//! Used by [`StateService`][1] and [`ReadStateService`][2] to read from the
//! best [`Chain`][5] in the [`NonFinalizedState`][3], and the database in the
//! [`FinalizedState`][4].
//!
//! [1]: super::StateService
//! [2]: super::ReadStateService
//! [3]: super::non_finalized_state::NonFinalizedState
//! [4]: super::finalized_state::FinalizedState
//! [5]: super::Chain
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::{RangeBounds, RangeInclusive},
    sync::Arc,
};

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    block::{self, Block, Height},
    orchard,
    parameters::Network,
    sapling,
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

// Blocks and Transactions

/// Returns the [`Block`] with [`block::Hash`](zebra_chain::block::Hash) or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
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
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}

/// Returns the [`block::Header`] with [`block::Hash`](zebra_chain::block::Hash) or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn block_header<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<block::Header>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.header.clone())
        .or_else(|| db.block_header(hash_or_height))
}

/// Returns the [`Transaction`] with [`transaction::Hash`], if it exists in the
/// non-finalized `chain` or finalized `db`.
pub(crate) fn transaction<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<(Arc<Transaction>, Height)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since transactions are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first. (`chain` is always in
    // memory, but `db` stores transactions on disk, with a memory cache.)
    chain
        .and_then(|chain| {
            chain
                .as_ref()
                .transaction(hash)
                .map(|(tx, height)| (tx.clone(), height))
        })
        .or_else(|| db.transaction(hash))
}

// FindBlockHeaders / FindBlockHashes

/// Returns the tip of `chain`.
/// If there is no chain, returns the tip of `db`.
pub(crate) fn tip_height<C>(chain: Option<C>, db: &ZebraDb) -> Option<Height>
where
    C: AsRef<Chain>,
{
    chain
        .map(|chain| chain.as_ref().non_finalized_tip_height())
        .or_else(|| db.finalized_tip_height())
}

/// Return the height for the block at `hash`, if `hash` is in the chain.
pub fn height_by_hash<C>(chain: Option<C>, db: &ZebraDb, hash: block::Hash) -> Option<Height>
where
    C: AsRef<Chain>,
{
    chain
        .and_then(|chain| chain.as_ref().height_by_hash(hash))
        .or_else(|| db.height(hash))
}

/// Return the hash for the block at `height`, if `height` is in the chain.
pub fn hash_by_height<C>(chain: Option<C>, db: &ZebraDb, height: Height) -> Option<block::Hash>
where
    C: AsRef<Chain>,
{
    chain
        .and_then(|chain| chain.as_ref().hash_by_height(height))
        .or_else(|| db.hash(height))
}

/// Return true if `hash` is in the chain.
pub fn chain_contains_hash<C>(chain: Option<C>, db: &ZebraDb, hash: block::Hash) -> bool
where
    C: AsRef<Chain>,
{
    chain
        .map(|chain| chain.as_ref().height_by_hash.contains_key(&hash))
        .unwrap_or(false)
        || db.contains_hash(hash)
}

/// Find the first hash that's in the peer's `known_blocks` and the chain.
///
/// Returns `None` if:
///   * there is no matching hash in the chain, or
///   * the state is empty.
fn find_chain_intersection<C>(
    chain: Option<C>,
    db: &ZebraDb,
    known_blocks: Vec<block::Hash>,
) -> Option<block::Hash>
where
    C: AsRef<Chain>,
{
    // We can get a block locator request before we have downloaded the genesis block
    if chain.is_none() && db.is_empty() {
        return None;
    }

    let chain = chain.as_ref();

    known_blocks
        .iter()
        .find(|&&hash| chain_contains_hash(chain, db, hash))
        .cloned()
}

/// Returns a range of [`Height`]s in the chain,
/// starting after the `intersection` hash on the chain.
///
/// See [`find_chain_hashes()`] for details.
fn find_chain_height_range<C>(
    chain: Option<C>,
    db: &ZebraDb,
    intersection: Option<block::Hash>,
    stop: Option<block::Hash>,
    max_len: u32,
) -> impl RangeBounds<u32> + Iterator<Item = u32>
where
    C: AsRef<Chain>,
{
    #[allow(clippy::reversed_empty_ranges)]
    const EMPTY_RANGE: RangeInclusive<u32> = 1..=0;

    assert!(max_len > 0, "max_len must be at least 1");

    let chain = chain.as_ref();

    // We can get a block locator request before we have downloaded the genesis block
    let chain_tip_height = if let Some(height) = tip_height(chain, db) {
        height
    } else {
        tracing::debug!(
            response_len = ?0,
            "responding to peer GetBlocks or GetHeaders with empty state",
        );

        return EMPTY_RANGE;
    };

    // Find the intersection height
    let intersection_height = match intersection {
        Some(intersection_hash) => match height_by_hash(chain, db, intersection_hash) {
            Some(intersection_height) => Some(intersection_height),

            // A recently committed block dropped the intersection we previously found
            None => {
                info!(
                    ?intersection,
                    ?stop,
                    ?max_len,
                    "state found intersection but then dropped it, ignoring request",
                );
                return EMPTY_RANGE;
            }
        },
        // There is no intersection
        None => None,
    };

    // Now find the start and maximum heights
    let (start_height, max_height) = match intersection_height {
        // start after the intersection_height, and return max_len hashes or headers
        Some(intersection_height) => (
            Height(intersection_height.0 + 1),
            Height(intersection_height.0 + max_len),
        ),
        // start at genesis, and return max_len hashes or headers
        None => (Height(0), Height(max_len - 1)),
    };

    let stop_height = stop.and_then(|hash| height_by_hash(chain, db, hash));

    // Compute the final height, making sure it is:
    //   * at or below our chain tip, and
    //   * at or below the height of the stop hash.
    let final_height = std::cmp::min(max_height, chain_tip_height);
    let final_height = stop_height
        .map(|stop_height| std::cmp::min(final_height, stop_height))
        .unwrap_or(final_height);

    // TODO: implement Step for Height, when Step stabilises
    //       https://github.com/rust-lang/rust/issues/42168
    let height_range = start_height.0..=final_height.0;
    let response_len = height_range.clone().into_iter().count();

    tracing::debug!(
        ?start_height,
        ?final_height,
        ?response_len,
        ?chain_tip_height,
        ?stop_height,
        ?intersection_height,
        ?intersection,
        ?stop,
        ?max_len,
        "responding to peer GetBlocks or GetHeaders",
    );

    // Check the function implements the Find protocol
    assert!(
        response_len <= max_len.try_into().expect("fits in usize"),
        "a Find response must not exceed the maximum response length",
    );

    height_range
}

/// Returns a list of [`block::Hash`]es in the chain,
/// following the `intersection` with the chain.
///
///
/// See [`find_chain_hashes()`] for details.
fn collect_chain_hashes<C>(
    chain: Option<C>,
    db: &ZebraDb,
    intersection: Option<block::Hash>,
    stop: Option<block::Hash>,
    max_len: u32,
) -> Vec<block::Hash>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();

    let height_range = find_chain_height_range(chain, db, intersection, stop, max_len);

    // All the hashes should be in the chain.
    // If they are not, we don't want to return them.
    let hashes: Vec<block::Hash> = height_range.into_iter().map_while(|height| {
            let hash = hash_by_height(chain, db, Height(height));

        // A recently committed block dropped the intersection we previously found.
            if hash.is_none() {
                info!(
                    ?intersection,
                    ?stop,
                    ?max_len,
                    "state found height range, but then partially dropped it, returning partial response",
                );
            }

            tracing::trace!(
                ?hash,
                ?height,
                ?intersection,
                ?stop,
                ?max_len,
                "adding hash to peer Find response",
            );

            hash
        }).collect();

    // Check the function implements the Find protocol
    assert!(
        intersection
            .map(|hash| !hashes.contains(&hash))
            .unwrap_or(true),
        "the list must not contain the intersection hash",
    );

    if let (Some(stop), Some((_, hashes_except_last))) = (stop, hashes.split_last()) {
        assert!(
            !hashes_except_last.contains(&stop),
            "if the stop hash is in the list, it must be the final hash",
        );
    }

    hashes
}

/// Returns a list of [`block::Header`]s in the chain,
/// following the `intersection` with the chain.
///
/// See [`find_chain_hashes()`] for details.
fn collect_chain_headers<C>(
    chain: Option<C>,
    db: &ZebraDb,
    intersection: Option<block::Hash>,
    stop: Option<block::Hash>,
    max_len: u32,
) -> Vec<Arc<block::Header>>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();

    let height_range = find_chain_height_range(chain, db, intersection, stop, max_len);

    // We don't check that this function implements the Find protocol,
    // because fetching extra hashes (or re-calculating hashes) is expensive.
    // (This was one of the most expensive and longest-running functions in the state.)

    // All the headers should be in the chain.
    // If they are not, we don't want to return them.
    height_range.into_iter().map_while(|height| {
            let header = block_header(chain, db, Height(height).into());

            // A recently committed block dropped the intersection we previously found
            if header.is_none() {
                info!(
                    ?intersection,
                    ?stop,
                    ?max_len,
                    "state found height range, but then partially dropped it, returning partial response",
                );
            }

            tracing::trace!(
                ?height,
                ?intersection,
                ?stop,
                ?max_len,
                "adding header to peer Find response",
            );

            header
        }).collect()
}

/// Finds the first hash that's in the peer's `known_blocks` and the chain.
/// Returns a list of hashes that follow that intersection, from the chain.
///
/// Starts from the first matching hash in the chain, ignoring all other hashes in
/// `known_blocks`. If there is no matching hash in the chain, starts from the genesis
/// hash.
///
/// Includes finalized and non-finalized blocks.
///
/// Stops the list of hashes after:
///   * adding the tip,
///   * adding the `stop` hash to the list, if it is in the chain, or
///   * adding 500 hashes to the list.
///
/// Returns an empty list if the state is empty,
/// and a partial or empty list if the found heights are concurrently modified.
pub fn find_chain_hashes<C>(
    chain: Option<C>,
    db: &ZebraDb,
    known_blocks: Vec<block::Hash>,
    stop: Option<block::Hash>,
    max_len: u32,
) -> Vec<block::Hash>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();
    let intersection = find_chain_intersection(chain, db, known_blocks);

    collect_chain_hashes(chain, db, intersection, stop, max_len)
}

/// Finds the first hash that's in the peer's `known_blocks` and the chain.
/// Returns a list of headers that follow that intersection, from the chain.
///
/// See [`find_chain_hashes()`] for details.
pub fn find_chain_headers<C>(
    chain: Option<C>,
    db: &ZebraDb,
    known_blocks: Vec<block::Hash>,
    stop: Option<block::Hash>,
    max_len: u32,
) -> Vec<Arc<block::Header>>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();
    let intersection = find_chain_intersection(chain, db, known_blocks);

    collect_chain_headers(chain, db, intersection, stop, max_len)
}

// Note Commitment Trees

/// Returns the Sapling
/// [`NoteCommitmentTree`](sapling::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn sapling_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<sapling::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since sapling treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().sapling_tree(hash_or_height))
        .or_else(|| db.sapling_tree(hash_or_height))
}

/// Returns the Orchard
/// [`NoteCommitmentTree`](orchard::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn orchard_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<orchard::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since orchard treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().orchard_tree(hash_or_height))
        .or_else(|| db.orchard_tree(hash_or_height))
}

// Address Balance

/// Returns the total transparent balance for the supplied [`transparent::Address`]es.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`, returns zero.
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

// Address UTXOs

/// Returns the unspent transparent outputs (UTXOs) for the supplied [`transparent::Address`]es,
/// in chain order; and the transaction IDs for the transactions containing those UTXOs.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`,
/// returns an empty list.
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
    let address_count = addresses.len();

    // Retry the finalized UTXO query if it was interrupted by a finalizing block,
    // and the non-finalized chain doesn't overlap the changed heights.
    for attempt in 0..=FINALIZED_ADDRESS_INDEX_RETRIES {
        debug!(?attempt, ?address_count, "starting address UTXO query");

        let (finalized_utxos, finalized_tip_range) = finalized_transparent_utxos(db, &addresses);

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
            Ok((created_chain_utxos, spent_chain_utxos)) => {
                debug!(
                    chain_utxo_count = ?created_chain_utxos.len(),
                    chain_utxo_spent = ?spent_chain_utxos.len(),
                    ?address_count,
                    ?attempt,
                    "chain address UTXO response",
                );

                let utxos =
                    apply_utxo_changes(finalized_utxos, created_chain_utxos, spent_chain_utxos);
                let tx_ids = lookup_tx_ids_for_utxos(chain, db, &addresses, &utxos);

                debug!(
                    full_utxo_count = ?utxos.len(),
                    tx_id_count = ?tx_ids.len(),
                    ?address_count,
                    ?attempt,
                    "full address UTXO response",
                );

                return Ok(AddressUtxos::new(network, utxos, tx_ids));
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
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // But we can compensate for deleted UTXOs by applying the overlapping non-finalized UTXO changes.

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
        if finalized_tip_status.is_ok() {
            debug!(
                ?finalized_tip_status,
                ?required_min_non_finalized_root,
                ?finalized_tip_range,
                ?address_count,
                "chain address UTXO query: \
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

                return Ok(Default::default());
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

    Ok(chain.partial_transparent_utxo_changes(addresses))
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

// Address TX IDs

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
