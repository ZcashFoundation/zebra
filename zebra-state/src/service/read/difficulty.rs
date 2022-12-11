//! Get context and calculate difficulty for the next block.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block, Hash, Height},
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade, POST_BLOSSOM_POW_TARGET_SPACING},
    serialization::{DateTime32, Duration32},
    work::difficulty::{CompactDifficulty, PartialCumulativeWork},
};

use crate::{
    service::{
        any_ancestor_blocks,
        check::{
            difficulty::{BLOCK_MAX_TIME_SINCE_MEDIAN, POW_ADJUSTMENT_BLOCK_SPAN},
            AdjustedDifficulty,
        },
        finalized_state::ZebraDb,
        read::{self, tree::history_tree, FINALIZED_STATE_QUERY_RETRIES},
        NonFinalizedState,
    },
    BoxError, GetBlockTemplateChainInfo,
};

/// Returns the [`GetBlockTemplateChainInfo`] for the current best chain.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
/// - If a consistency check fails `RETRIES` times.
pub fn get_block_template_chain_info(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    network: Network,
) -> Result<GetBlockTemplateChainInfo, BoxError> {
    let mut relevant_chain_and_history_tree_result =
        relevant_chain_and_history_tree(non_finalized_state, db);

    // Retry the finalized state query if it was interrupted by a finalizing block.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..FINALIZED_STATE_QUERY_RETRIES {
        if relevant_chain_and_history_tree_result.is_ok() {
            break;
        }

        relevant_chain_and_history_tree_result =
            relevant_chain_and_history_tree(non_finalized_state, db);
    }

    let (tip_height, tip_hash, relevant_chain, history_tree) =
        relevant_chain_and_history_tree_result?;

    Ok(difficulty_time_and_history_tree(
        relevant_chain,
        tip_height,
        tip_hash,
        network,
        history_tree,
    ))
}

/// Accepts a `non_finalized_state`, [`ZebraDb`], `num_blocks`, and a block hash to start at.
///
/// Iterates over up to the last `num_blocks` blocks, summing up their total work.
/// Divides that total by the number of seconds between the timestamp of the
/// first block in the iteration and 1 block below the last block.
///
/// Returns the solution rate per second for the current best chain, or `None` if
/// the `start_hash` and at least 1 block below it are not found in the chain.
pub fn solution_rate(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    num_blocks: usize,
    start_hash: Hash,
) -> Option<u128> {
    // Take 1 extra block for calculating the number of seconds between when mining on the first block likely started.
    // The work for the last block in this iterator is not added to `total_work`.
    let mut block_iter = any_ancestor_blocks(non_finalized_state, db, start_hash)
        .take(num_blocks.checked_add(1).unwrap_or(num_blocks))
        .peekable();

    let get_work = |block: Arc<Block>| {
        block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated")
    };

    let block = block_iter.next()?;
    let last_block_time = block.header.time;

    let mut total_work: PartialCumulativeWork = get_work(block).into();

    loop {
        // Return `None` if the iterator doesn't yield a second item.
        let block = block_iter.next()?;

        if block_iter.peek().is_some() {
            // Add the block's work to `total_work` if it's not the last item in the iterator.
            // The last item in the iterator is only used to estimate when mining on the first block
            // in the window of `num_blocks` likely started.
            total_work += get_work(block);
        } else {
            let first_block_time = block.header.time;
            let duration_between_first_and_last_block = last_block_time - first_block_time;
            return Some(
                total_work.as_u128() / duration_between_first_and_last_block.num_seconds() as u128,
            );
        }
    }
}

/// Do a consistency check by checking the finalized tip before and after all other database queries.
/// Returns and error if the tip obtained before and after is not the same.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
fn relevant_chain_and_history_tree(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> Result<
    (
        Height,
        block::Hash,
        [Arc<Block>; POW_ADJUSTMENT_BLOCK_SPAN],
        Arc<HistoryTree>,
    ),
    BoxError,
> {
    let state_tip_before_queries = read::best_tip(non_finalized_state, db).ok_or_else(|| {
        BoxError::from("Zebra's state is empty, wait until it syncs to the chain tip")
    })?;

    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, state_tip_before_queries.1);
    let relevant_chain: Vec<_> = relevant_chain
        .into_iter()
        .take(POW_ADJUSTMENT_BLOCK_SPAN)
        .collect();
    let relevant_chain = relevant_chain.try_into().map_err(|_error| {
        "Zebra's state only has a few blocks, wait until it syncs to the chain tip"
    })?;

    let history_tree = history_tree(
        non_finalized_state.best_chain(),
        db,
        state_tip_before_queries.into(),
    )
    .expect("tip hash should exist in the chain");

    let state_tip_after_queries =
        read::best_tip(non_finalized_state, db).expect("already checked for an empty tip");

    if state_tip_before_queries != state_tip_after_queries {
        return Err("Zebra is committing too many blocks to the state, \
                    wait until it syncs to the chain tip"
            .into());
    }

    Ok((
        state_tip_before_queries.0,
        state_tip_before_queries.1,
        relevant_chain,
        history_tree,
    ))
}

/// Returns the [`GetBlockTemplateChainInfo`] for the current best chain.
///
/// See [`get_block_template_chain_info()`] for details.
fn difficulty_time_and_history_tree(
    relevant_chain: [Arc<Block>; POW_ADJUSTMENT_BLOCK_SPAN],
    tip_height: Height,
    tip_hash: block::Hash,
    network: Network,
    history_tree: Arc<HistoryTree>,
) -> GetBlockTemplateChainInfo {
    let relevant_data: Vec<(CompactDifficulty, DateTime<Utc>)> = relevant_chain
        .iter()
        .map(|block| (block.header.difficulty_threshold, block.header.time))
        .collect();

    let cur_time = DateTime32::now();

    // Get the median-time-past, which doesn't depend on the time or the previous block height.
    // `context` will always have the correct length, because this function takes an array.
    //
    // TODO: split out median-time-past into its own struct?
    let median_time_past = AdjustedDifficulty::new_from_header_time(
        cur_time.into(),
        tip_height,
        network,
        relevant_data.clone(),
    )
    .median_time_past();

    // > For each block other than the genesis block , nTime MUST be strictly greater than
    // > the median-time-past of that block.
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let median_time_past =
        DateTime32::try_from(median_time_past).expect("valid blocks have in-range times");

    let min_time = median_time_past
        .checked_add(Duration32::from_seconds(1))
        .expect("a valid block time plus a small constant is in-range");

    // > For each block at block height 2 or greater on Mainnet, or block height 653606 or greater on Testnet, nTime
    // > MUST be less than or equal to the median-time-past of that block plus 90 * 60 seconds.
    //
    // We ignore the height as we are checkpointing on Canopy or higher in Mainnet and Testnet.
    let max_time = median_time_past
        .checked_add(Duration32::from_seconds(BLOCK_MAX_TIME_SINCE_MEDIAN))
        .expect("a valid block time plus a small constant is in-range");

    let cur_time = cur_time.clamp(min_time, max_time);

    // Now that we have a valid time, get the difficulty for that time.
    let difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        cur_time.into(),
        tip_height,
        network,
        relevant_data.iter().cloned(),
    );

    let mut result = GetBlockTemplateChainInfo {
        tip_hash,
        tip_height,
        history_tree,
        expected_difficulty: difficulty_adjustment.expected_difficulty_threshold(),
        cur_time,
        min_time,
        max_time,
    };

    adjust_difficulty_and_time_for_testnet(&mut result, network, tip_height, relevant_data);

    result
}

/// Adjust the difficulty and time for the testnet minimum difficulty rule.
fn adjust_difficulty_and_time_for_testnet(
    result: &mut GetBlockTemplateChainInfo,
    network: Network,
    previous_block_height: Height,
    relevant_data: Vec<(CompactDifficulty, DateTime<Utc>)>,
) {
    if network == Network::Mainnet {
        return;
    }

    // On testnet, changing the block time can also change the difficulty,
    // due to the minimum difficulty consensus rule:
    // > if the block time of a block at height `height ≥ 299188`
    // > is greater than 6 * PoWTargetSpacing(height) seconds after that of the preceding block,
    // > then the block is a minimum-difficulty block.
    //
    // The max time is always a minimum difficulty block, because the minimum difficulty
    // gap is 7.5 minutes, but the maximum gap is 90 minutes. This means that testnet blocks
    // have two valid time ranges with different difficulties:
    // * 1s - 7m30s: standard difficulty
    // * 7m31s - 90m: minimum difficulty
    //
    // In rare cases, this could make some testnet miners produce invalid blocks,
    // if they use the full 90 minute time gap in the consensus rules.
    // (The zcashd getblocktemplate RPC reference doesn't have a max_time field,
    // so there is no standard way of telling miners that the max_time is smaller.)
    //
    // So Zebra adjusts the min or max times to produce a valid time range for the difficulty.
    // There is still a small chance that miners will produce an invalid block, if they are
    // just below the max time, and don't check it.

    let previous_block_time = relevant_data.last().expect("has at least one block").1;
    let previous_block_time: DateTime32 = previous_block_time
        .try_into()
        .expect("valid blocks have in-range times");

    let minimum_difficulty_spacing =
        NetworkUpgrade::minimum_difficulty_spacing_for_height(network, previous_block_height)
            .expect("just checked testnet, and the RPC returns an error for low heights");
    let minimum_difficulty_spacing: Duration32 = minimum_difficulty_spacing
        .try_into()
        .expect("small positive values are in-range");

    // The first minimum difficulty time is strictly greater than the spacing.
    let std_difficulty_max_time = previous_block_time
        .checked_add(minimum_difficulty_spacing)
        .expect("a valid block time plus a small constant is in-range");
    let min_difficulty_min_time = std_difficulty_max_time
        .checked_add(Duration32::from_seconds(1))
        .expect("a valid block time plus a small constant is in-range");

    // If a miner is likely to find a block with the cur_time and standard difficulty
    //
    // We don't need to undo the clamping here:
    // - if cur_time is clamped to min_time, then we're more likely to have a minimum
    //    difficulty block, which makes mining easier;
    // - if cur_time gets clamped to max_time, this is already a minimum difficulty block.
    let local_std_difficulty_limit = std_difficulty_max_time
        .checked_sub(Duration32::from_seconds(
            POST_BLOSSOM_POW_TARGET_SPACING * 2,
        ))
        .expect("a valid block time minus a small constant is in-range");

    if result.cur_time <= local_std_difficulty_limit {
        // Standard difficulty: the max time needs to exclude min difficulty blocks
        result.max_time = std_difficulty_max_time;
    } else {
        // Minimum difficulty: the min and cur time need to exclude min difficulty blocks
        result.min_time = min_difficulty_min_time;
        if result.cur_time < min_difficulty_min_time {
            result.cur_time = min_difficulty_min_time;
        }

        // And then the difficulty needs to be updated for cur_time
        result.expected_difficulty = AdjustedDifficulty::new_from_header_time(
            result.cur_time.into(),
            previous_block_height,
            network,
            relevant_data.iter().cloned(),
        )
        .expected_difficulty_threshold();
    }
}
