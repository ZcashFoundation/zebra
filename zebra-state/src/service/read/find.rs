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

use zebra_chain::block::{self, Height};

use crate::{
    constants,
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::block::block_header,
    },
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
