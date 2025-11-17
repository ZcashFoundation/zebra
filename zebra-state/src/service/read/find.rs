//! Finding and reading block hashes and headers, in response to peer requests.
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
    iter,
    ops::{RangeBounds, RangeInclusive},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use zebra_chain::{
    amount::NonNegative,
    block::{self, Block, Hash, Height},
    parameters::NetworkUpgrade,
    serialization::DateTime32,
    value_balance::ValueBalance,
    work::difficulty::U256,
};

use crate::{
    constants,
    service::{
        block_iter::any_ancestor_blocks,
        check::{difficulty::POW_MEDIAN_BLOCK_SPAN, AdjustedDifficulty},
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::{self, block::block_header, FINALIZED_STATE_QUERY_RETRIES},
    },
    BoxError, KnownBlock,
};

#[cfg(test)]
mod tests;

/// Returns the tip of the best chain in the non-finalized or finalized state.
pub fn best_tip(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> Option<(block::Height, block::Hash)> {
    tip(non_finalized_state.best_chain(), db)
}

/// Returns the tip of `chain`.
/// If there is no chain, returns the tip of `db`.
pub fn tip<C>(chain: Option<C>, db: &ZebraDb) -> Option<(Height, block::Hash)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // If there is an overlap between the non-finalized and finalized states,
    // where the finalized tip is above the non-finalized tip,
    // Zebra is receiving a lot of blocks, or this request has been delayed for a long time,
    // so it is acceptable to return either tip.
    chain
        .map(|chain| chain.as_ref().non_finalized_tip())
        .or_else(|| db.tip())
}

/// Returns the tip [`Height`] of `chain`.
/// If there is no chain, returns the tip of `db`.
pub fn tip_height<C>(chain: Option<C>, db: &ZebraDb) -> Option<Height>
where
    C: AsRef<Chain>,
{
    tip(chain, db).map(|(height, _hash)| height)
}

/// Returns the tip [`block::Hash`] of `chain`.
/// If there is no chain, returns the tip of `db`.
#[allow(dead_code)]
pub fn tip_hash<C>(chain: Option<C>, db: &ZebraDb) -> Option<block::Hash>
where
    C: AsRef<Chain>,
{
    tip(chain, db).map(|(_height, hash)| hash)
}

/// Returns the tip of `chain` with its [`ValueBalance`].
/// If there is no chain, returns the tip of `db`.
pub fn tip_with_value_balance<C>(
    chain: Option<C>,
    db: &ZebraDb,
) -> Result<Option<(Height, block::Hash, ValueBalance<NonNegative>)>, BoxError>
where
    C: AsRef<Chain>,
{
    match chain.map(|chain| chain.as_ref().non_finalized_tip_with_value_balance()) {
        tip_with_value_balance @ Some(_) => Ok(tip_with_value_balance),
        None => {
            // Retry the finalized state query if it was interrupted by a finalizing block.
            //
            // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
            for _ in 0..=FINALIZED_STATE_QUERY_RETRIES {
                let tip @ Some((tip_height, tip_hash)) = db.tip() else {
                    return Ok(None);
                };

                let value_balance = db.finalized_value_pool();

                if tip == db.tip() {
                    return Ok(Some((tip_height, tip_hash, value_balance)));
                }
            }

            Err("Zebra is committing too many blocks to the state, \
                    wait until it syncs to the chain tip"
                .into())
        }
    }
}

/// Return the depth of block `hash` from the chain tip.
/// Searches `chain` for `hash`, then searches `db`.
pub fn depth<C>(chain: Option<C>, db: &ZebraDb, hash: block::Hash) -> Option<u32>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();

    // # Correctness
    //
    // It is ok to do this lookup in two different calls. Finalized state updates
    // can only add overlapping blocks, and hashes are unique.

    let tip = tip_height(chain, db)?;
    let height = height_by_hash(chain, db, hash)?;

    Some(tip.0 - height.0)
}

/// Returns the location of the block if present in the non-finalized state.
/// Returns None if the block hash is not found in the non-finalized state.
pub fn non_finalized_state_contains_block_hash(
    non_finalized_state: &NonFinalizedState,
    hash: block::Hash,
) -> Option<KnownBlock> {
    let mut chains_iter = non_finalized_state.chain_iter();
    let is_hash_in_chain = |chain: &Arc<Chain>| chain.contains_block_hash(hash);

    // Equivalent to `chain_set.iter().next_back()` in `NonFinalizedState.best_chain()` method.
    let best_chain = chains_iter.next();

    match best_chain.map(is_hash_in_chain) {
        Some(true) => Some(KnownBlock::BestChain),
        Some(false) if chains_iter.any(is_hash_in_chain) => Some(KnownBlock::SideChain),
        Some(false) | None => None,
    }
}

/// Returns the location of the block if present in the finalized state.
/// Returns None if the block hash is not found in the finalized state.
pub fn finalized_state_contains_block_hash(db: &ZebraDb, hash: block::Hash) -> Option<KnownBlock> {
    db.contains_hash(hash).then_some(KnownBlock::BestChain)
}

/// Return the height for the block at `hash`, if `hash` is in `chain` or `db`.
pub fn height_by_hash<C>(chain: Option<C>, db: &ZebraDb, hash: block::Hash) -> Option<Height>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Finalized state updates can only add overlapping blocks, and hashes are unique.

    chain
        .and_then(|chain| chain.as_ref().height_by_hash(hash))
        .or_else(|| db.height(hash))
}

/// Return the hash for the block at `height`, if `height` is in `chain` or `db`.
pub fn hash_by_height<C>(chain: Option<C>, db: &ZebraDb, height: Height) -> Option<block::Hash>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Finalized state updates can only add overlapping blocks, and heights are unique
    // in the current `chain`.
    //
    // If there is an overlap between the non-finalized and finalized states,
    // where the finalized tip is above the non-finalized tip,
    // Zebra is receiving a lot of blocks, or this request has been delayed for a long time,
    // so it is acceptable to return hashes from either chain.

    chain
        .and_then(|chain| chain.as_ref().hash_by_height(height))
        .or_else(|| db.hash(height))
}

/// Return true if `hash` is in `chain` or `db`.
pub fn chain_contains_hash<C>(chain: Option<C>, db: &ZebraDb, hash: block::Hash) -> bool
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Finalized state updates can only add overlapping blocks, and hashes are unique.
    //
    // If there is an overlap between the non-finalized and finalized states,
    // where the finalized tip is above the non-finalized tip,
    // Zebra is receiving a lot of blocks, or this request has been delayed for a long time,
    // so it is acceptable to return hashes from either chain.

    chain
        .map(|chain| chain.as_ref().height_by_hash.contains_key(&hash))
        .unwrap_or(false)
        || db.contains_hash(hash)
}

/// Create a block locator from `chain` and `db`.
///
/// A block locator is used to efficiently find an intersection of two node's chains.
/// It contains a list of block hashes at decreasing heights, skipping some blocks,
/// so that any intersection can be located, no matter how long or different the chains are.
pub fn block_locator<C>(chain: Option<C>, db: &ZebraDb) -> Option<Vec<block::Hash>>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();

    // # Correctness
    //
    // It is ok to do these lookups using multiple database calls. Finalized state updates
    // can only add overlapping blocks, and hashes are unique.
    //
    // If there is an overlap between the non-finalized and finalized states,
    // where the finalized tip is above the non-finalized tip,
    // Zebra is receiving a lot of blocks, or this request has been delayed for a long time,
    // so it is acceptable to return a set of hashes from multiple chains.
    //
    // Multiple heights can not map to the same hash, even in different chains,
    // because the block height is covered by the block hash,
    // via the transaction merkle tree commitments.
    let tip_height = tip_height(chain, db)?;

    let heights = block_locator_heights(tip_height);
    let mut hashes = Vec::with_capacity(heights.len());

    for height in heights {
        if let Some(hash) = hash_by_height(chain, db, height) {
            hashes.push(hash);
        }
    }

    Some(hashes)
}

/// Get the heights of the blocks for constructing a block_locator list.
///
/// Zebra uses a decreasing list of block heights, starting at the tip, and skipping some heights.
/// See [`block_locator()`] for details.
pub fn block_locator_heights(tip_height: block::Height) -> Vec<block::Height> {
    // The initial height in the returned `vec` is the tip height,
    // and the final height is `MAX_BLOCK_REORG_HEIGHT` below the tip.
    //
    // The initial distance between heights is 1, and it doubles between each subsequent height.
    // So the number of returned heights is approximately `log_2(MAX_BLOCK_REORG_HEIGHT)`.

    // Limit the maximum locator depth.
    let min_locator_height = tip_height
        .0
        .saturating_sub(constants::MAX_BLOCK_REORG_HEIGHT);

    // Create an exponentially decreasing set of heights.
    let exponential_locators = iter::successors(Some(1u32), |h| h.checked_mul(2))
        .flat_map(move |step| tip_height.0.checked_sub(step));

    // Start at the tip, add decreasing heights, and end MAX_BLOCK_REORG_HEIGHT below the tip.
    let locators = iter::once(tip_height.0)
        .chain(exponential_locators)
        .take_while(move |&height| height > min_locator_height)
        .chain(iter::once(min_locator_height))
        .map(block::Height)
        .collect();

    tracing::debug!(
        ?tip_height,
        ?min_locator_height,
        ?locators,
        "created block locator"
    );

    locators
}

/// Find the first hash that's in the peer's `known_blocks`, and in `chain` or `db`.
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
    let response_len = height_range.clone().count();

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
///   * adding `max_len` hashes to the list.
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
    // # Correctness
    //
    // See the note in `block_locator()`.

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
    // # Correctness
    //
    // Headers are looked up by their hashes using a unique mapping,
    // so it is not possible for multiple hashes to look up the same header,
    // even across different chains.
    //
    // See also the note in `block_locator()`.

    let chain = chain.as_ref();
    let intersection = find_chain_intersection(chain, db, known_blocks);

    collect_chain_headers(chain, db, intersection, stop, max_len)
}

/// Returns the median-time-past of the *next* block to be added to the best chain in
/// `non_finalized_state` or `db`.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
pub fn next_median_time_past(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> Result<DateTime32, BoxError> {
    let mut best_relevant_chain_result = best_relevant_chain(non_finalized_state, db);

    // Retry the finalized state query if it was interrupted by a finalizing block.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..FINALIZED_STATE_QUERY_RETRIES {
        if best_relevant_chain_result.is_ok() {
            break;
        }

        best_relevant_chain_result = best_relevant_chain(non_finalized_state, db);
    }

    Ok(calculate_median_time_past(best_relevant_chain_result?))
}

/// Do a consistency check by checking the finalized tip before and after all other database queries.
///
/// Returns recent blocks in reverse height order from the tip.
/// Returns an error if the tip obtained before and after is not the same.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
fn best_relevant_chain(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> Result<Vec<Arc<Block>>, BoxError> {
    let state_tip_before_queries = read::best_tip(non_finalized_state, db).ok_or_else(|| {
        BoxError::from("Zebra's state is empty, wait until it syncs to the chain tip")
    })?;

    let best_relevant_chain =
        any_ancestor_blocks(non_finalized_state, db, state_tip_before_queries.1);
    let best_relevant_chain: Vec<_> = best_relevant_chain
        .into_iter()
        .take(POW_MEDIAN_BLOCK_SPAN)
        .collect();

    if best_relevant_chain.is_empty() {
        return Err("missing genesis block, wait until it is committed".into());
    };

    let state_tip_after_queries =
        read::best_tip(non_finalized_state, db).expect("already checked for an empty tip");

    if state_tip_before_queries != state_tip_after_queries {
        return Err("Zebra is committing too many blocks to the state, \
                    wait until it syncs to the chain tip"
            .into());
    }

    Ok(best_relevant_chain)
}

/// Returns the median-time-past for the provided `relevant_chain`.
///
/// The `relevant_chain` has blocks in reverse height order.
///
/// See [`next_median_time_past()`] for details.
pub(crate) fn calculate_median_time_past(relevant_chain: Vec<Arc<Block>>) -> DateTime32 {
    let relevant_data: Vec<DateTime<Utc>> = relevant_chain
        .iter()
        .map(|block| block.header.time)
        .collect();

    // > Define the median-time-past of a block to be the median of the nTime fields of the
    // > preceding PoWMedianBlockSpan blocks (or all preceding blocks if there are fewer than
    // > PoWMedianBlockSpan). The median-time-past of a genesis block is not defined.
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let median_time_past = AdjustedDifficulty::median_time(relevant_data);

    DateTime32::try_from(median_time_past).expect("valid blocks have in-range times")
}

/// Return the height and hash of the first block with cumulative total work equal or greater than `threshold`.
///
/// Returns `None` if the threshold is greater than the total work of the finalized state, or if
/// history trees are not present for this chain.
pub fn first_block_with_total_work(db: &ZebraDb, threshold: U256) -> Option<(Height, Hash)> {
    // Search upgrades with history trees
    let network = db.network();
    let tip_height = db.finalized_tip_height()?;
    let current_upgrade = NetworkUpgrade::current(&network, tip_height);
    let history_tree_upgrades: Vec<NetworkUpgrade> = NetworkUpgrade::iter()
        .skip_while(|u| *u != NetworkUpgrade::Heartwood)
        .take_while(|u| *u != current_upgrade)
        .chain(std::iter::once(current_upgrade))
        .collect();

    // Edge case: try to match the very first Heartwood tree which would otherwise get skipped over
    let min_work_height = (NetworkUpgrade::Heartwood.activation_height(&network)? + 1)?;
    let min_work = db
        .history_tree_by_height(min_work_height)?
        .root_node()?
        .subtree_total_work();
    if threshold <= min_work {
        return Some((min_work_height, db.hash(min_work_height)?));
    }

    let mut total_work = U256::zero();

    // Find first upgrade that goes over the threshold
    for upgrade in history_tree_upgrades {
        let start_work = total_work;
        total_work += db
            .history_tree_by_upgrade(upgrade)?
            .root_node()?
            .subtree_total_work();
        if total_work >= threshold {
            let local_threshold = threshold - start_work;
            let mut start_height = upgrade.activation_height(&network)?;
            let mut end_height = if upgrade == current_upgrade {
                db.finalized_tip_height()?
            } else {
                upgrade.next_upgrade()?.activation_height(&network)?
            };

            // Binary search over subtree_total_work within this upgrade
            while start_height != end_height {
                let middle = Height(((start_height.as_usize() + end_height.as_usize()) / 2) as u32);
                let local_work = db
                    .history_tree_by_height(middle)?
                    .root_node()?
                    .subtree_total_work();

                if local_work >= local_threshold {
                    end_height = middle;
                } else {
                    start_height = (middle + 1)?;
                }
            }

            return Some((end_height, db.hash(end_height)?));
        }
    }

    // Total work is less than threshold
    None
}
