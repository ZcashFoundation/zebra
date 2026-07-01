//! Get context and calculate difficulty for the next block.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block, Hash, Height},
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, Duration32},
    work::difficulty::{CompactDifficulty, PartialCumulativeWork, Work},
};

use crate::{
    service::{
        any_ancestor_blocks,
        block_iter::any_chain_ancestor_iter,
        check::{
            difficulty::{
                BLOCK_MAX_TIME_SINCE_MEDIAN, POW_ADJUSTMENT_BLOCK_SPAN, POW_MEDIAN_BLOCK_SPAN,
            },
            AdjustedDifficulty,
        },
        finalized_state::ZebraDb,
        read::{
            self, find::calculate_median_time_past, tree::history_tree,
            FINALIZED_STATE_QUERY_RETRIES,
        },
        NonFinalizedState,
    },
    BoxError, GetBlockTemplateChainInfo,
};

/// Returns the [`GetBlockTemplateChainInfo`] for the current best chain.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
pub fn get_block_template_chain_info(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    network: &Network,
) -> Result<GetBlockTemplateChainInfo, BoxError> {
    let mut best_relevant_chain_and_history_tree_result =
        best_relevant_chain_and_history_tree(non_finalized_state, db);

    // Retry the finalized state query if it was interrupted by a finalizing block.
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..FINALIZED_STATE_QUERY_RETRIES {
        if best_relevant_chain_and_history_tree_result.is_ok() {
            break;
        }

        best_relevant_chain_and_history_tree_result =
            best_relevant_chain_and_history_tree(non_finalized_state, db);
    }

    let (best_tip_height, best_tip_hash, best_relevant_chain, best_tip_history_tree) =
        best_relevant_chain_and_history_tree_result?;

    Ok(difficulty_time_and_history_tree(
        best_relevant_chain,
        best_tip_height,
        best_tip_hash,
        network,
        best_tip_history_tree,
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
#[allow(unused)]
pub fn solution_rate(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    num_blocks: usize,
    start_hash: Hash,
) -> Option<u128> {
    // Take 1 extra header for calculating the number of seconds between when mining on the first
    // block likely started. The work for the extra header is not added to `total_work`.
    //
    // Since we can't take more headers than are actually in the chain, this automatically limits
    // `num_blocks` to the chain length, like `zcashd` does.
    let mut header_iter =
        any_chain_ancestor_iter::<block::Header>(non_finalized_state, db, start_hash)
            .take(num_blocks.checked_add(1).unwrap_or(num_blocks))
            .peekable();

    let get_work = |header: &block::Header| {
        header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated")
    };

    // If there are no blocks in the range, we can't return a useful result.
    let last_header = header_iter.peek()?;

    // Initialize the cumulative variables.
    let mut min_time = last_header.time;
    let mut max_time = last_header.time;

    let mut last_work = Work::zero();
    let mut total_work = PartialCumulativeWork::zero();

    for header in header_iter {
        min_time = min_time.min(header.time);
        max_time = max_time.max(header.time);

        last_work = get_work(&header);
        total_work += last_work;
    }

    // We added an extra header so we could estimate when mining on the first block
    // in the window of `num_blocks` likely started. But we don't want to add the work
    // for that header.
    total_work -= last_work;

    let work_duration = (max_time - min_time).num_seconds();

    // Avoid division by zero errors and negative average work.
    // This also handles the case where there's only one block in the range.
    if work_duration <= 0 {
        return None;
    }

    Some(total_work.as_u128() / work_duration as u128)
}

/// Do a consistency check by checking the finalized tip before and after all other database
/// queries.
///
/// Returns the best chain tip, recent blocks in reverse height order from the tip,
/// and the tip history tree.
/// Returns an error if the tip obtained before and after is not the same.
///
/// # Panics
///
/// - If we don't have enough blocks in the state.
fn best_relevant_chain_and_history_tree(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> Result<(Height, block::Hash, Vec<Arc<Block>>, Arc<HistoryTree>), BoxError> {
    let state_tip_before_queries = read::best_tip(non_finalized_state, db).ok_or_else(|| {
        BoxError::from("Zebra's state is empty, wait until it syncs to the chain tip")
    })?;

    let best_relevant_chain =
        any_ancestor_blocks(non_finalized_state, db, state_tip_before_queries.1);
    let best_relevant_chain: Vec<_> = best_relevant_chain
        .into_iter()
        .take(POW_ADJUSTMENT_BLOCK_SPAN)
        .collect();

    if best_relevant_chain.is_empty() {
        return Err("missing genesis block, wait until it is committed".into());
    };

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
        best_relevant_chain,
        history_tree,
    ))
}

/// Returns the [`GetBlockTemplateChainInfo`] for the supplied `relevant_chain`, tip, `network`,
/// and `history_tree`.
///
/// The `relevant_chain` has recent blocks in reverse height order from the tip.
///
/// See [`get_block_template_chain_info()`] for details.
fn difficulty_time_and_history_tree(
    relevant_chain: Vec<Arc<Block>>,
    tip_height: Height,
    tip_hash: block::Hash,
    network: &Network,
    history_tree: Arc<HistoryTree>,
) -> GetBlockTemplateChainInfo {
    let relevant_data: Vec<(CompactDifficulty, DateTime<Utc>)> = relevant_chain
        .iter()
        .map(|block| (block.header.difficulty_threshold, block.header.time))
        .collect();

    let cur_time = DateTime32::now();

    // > For each block other than the genesis block , nTime MUST be strictly greater than
    // > the median-time-past of that block.
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let median_time_past = calculate_median_time_past(
        relevant_chain
            .iter()
            .take(POW_MEDIAN_BLOCK_SPAN)
            .cloned()
            .collect(),
    );

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
    let expected_difficulty = difficulty_adjustment.expected_difficulty_threshold();

    let mut result = GetBlockTemplateChainInfo {
        tip_hash,
        tip_height,
        chain_history_root: history_tree.hash(),
        expected_difficulty,
        cur_time,
        min_time,
        max_time,
    };

    adjust_difficulty_and_time_for_testnet(&mut result, network, tip_height, relevant_data);

    result
}

/// Adjust the difficulty and time for the testnet minimum difficulty rule.
///
/// The `relevant_data` has recent block difficulties and times in reverse order from the tip.
fn adjust_difficulty_and_time_for_testnet(
    result: &mut GetBlockTemplateChainInfo,
    network: &Network,
    previous_block_height: Height,
    relevant_data: Vec<(CompactDifficulty, DateTime<Utc>)>,
) {
    if network == &Network::Mainnet {
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

    // The tip is the first relevant data block, because they are in reverse order.
    let previous_block_time = relevant_data.first().expect("has at least one block").1;
    let previous_block_time: DateTime32 = previous_block_time
        .try_into()
        .expect("valid blocks have in-range times");

    let Some(minimum_difficulty_spacing) =
        NetworkUpgrade::minimum_difficulty_spacing_for_height(network, previous_block_height)
    else {
        // Returns early if the testnet minimum difficulty consensus rule is not active
        return;
    };

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

    // Offer a minimum-difficulty template only once `cur_time` is strictly past
    // `previous_block_time + 6 * PoWTargetSpacing` (the latest time a standard-difficulty
    // block may use; a minimum-difficulty block's time must be strictly greater).
    //
    // A minimum-difficulty block is only consensus-valid if its time is more than
    // `6 * PoWTargetSpacing` after the previous block. Switching to a minimum-difficulty template
    // before `cur_time` has reached that point would require clamping `cur_time` up to `min_time`,
    // i.e. future-dating the block timestamp ahead of real time purely to obtain minimum difficulty.
    //
    // Zebra used to do this, switching 150 seconds early (`2 * PoWTargetSpacing` after Blossom),
    // via a former `EXTRA_TIME_TO_MINE_A_BLOCK` constant. On a chain with any sustained hashrate
    // that produced a stream of future-dated minimum-difficulty blocks, each of which sharply
    // reduces the difficulty (the difficulty average is over targets and includes
    // minimum-difficulty blocks), depressing the difficulty far below equilibrium. See
    // <https://github.com/zcash/zips/issues/1321>.
    //
    // This does not change the underlying difficulty averaging: a genuine gap of more than
    // `6 * PoWTargetSpacing` still yields a minimum-difficulty block that reduces the difficulty.
    // It only stops Zebra from proactively generating such blocks by future-dating timestamps.
    //
    // We don't need to undo the clamping here:
    // - if cur_time is clamped to min_time, then we're more likely to have a minimum
    //    difficulty block, which makes mining easier;
    // - if cur_time gets clamped to max_time, this is almost always a minimum difficulty block.
    if result.cur_time <= std_difficulty_max_time {
        // Standard difficulty: the cur and max time need to exclude min difficulty blocks

        // The maximum time can only be decreased, and only as far as min_time.
        // The old minimum is still required by other consensus rules.
        result.max_time = std_difficulty_max_time.clamp(result.min_time, result.max_time);

        // The current time only needs to be decreased if the max_time decreased past it.
        // Decreasing the current time can't change the difficulty.
        result.cur_time = result.cur_time.clamp(result.min_time, result.max_time);
    } else {
        // Minimum difficulty: the min and cur time need to exclude std difficulty blocks

        // The minimum time can only be increased, and only as far as max_time.
        // The old maximum is still required by other consensus rules.
        result.min_time = min_difficulty_min_time.clamp(result.min_time, result.max_time);

        // The current time only needs to be increased if the min_time increased past it.
        result.cur_time = result.cur_time.clamp(result.min_time, result.max_time);

        // And then the difficulty needs to be updated for cur_time.
        result.expected_difficulty = AdjustedDifficulty::new_from_header_time(
            result.cur_time.into(),
            previous_block_height,
            network,
            relevant_data.iter().cloned(),
        )
        .expected_difficulty_threshold();
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the Testnet minimum-difficulty template adjustment. They construct the
    //! [`GetBlockTemplateChainInfo`] and block context with fixed times, exercising
    //! `adjust_difficulty_and_time_for_testnet` deterministically without reading the real
    //! clock (the `DateTime32::now()` call lives only in its caller).

    use super::*;
    use zebra_chain::work::difficulty::ParameterDifficulty as _;

    // A Testnet height at which the minimum-difficulty rule is active (>= 299188) and the
    // target spacing is the post-Blossom 75 s, so the minimum-difficulty gap is 6 * 75 = 450 s.
    const ACTIVE_HEIGHT: Height = Height(2_000_000);
    const PREV: u32 = 1_600_000_000;
    const GAP: u32 = 6 * 75;

    /// Recent block difficulties and times in reverse order from the tip, with the most
    /// recent (first) block time equal to `PREV`. The difficulty thresholds are irrelevant
    /// to the minimum-difficulty rule under test.
    fn recent_block_data(network: &Network) -> Vec<(CompactDifficulty, DateTime<Utc>)> {
        let threshold = network.target_difficulty_limit().to_compact();
        (0..POW_ADJUSTMENT_BLOCK_SPAN)
            .map(|i| (threshold, DateTime32::from(PREV - i as u32).into()))
            .collect()
    }

    /// A [`GetBlockTemplateChainInfo`] with candidate time `cur_time` and a wide
    /// `[min_time, max_time]` window around `PREV`, so the only clamping under test comes
    /// from the minimum-difficulty adjustment. `expected_difficulty` starts at a sentinel.
    fn chain_info(cur_time: u32) -> GetBlockTemplateChainInfo {
        GetBlockTemplateChainInfo {
            tip_hash: Hash([0; 32]),
            tip_height: Height(0),
            chain_history_root: None,
            expected_difficulty: CompactDifficulty::default(),
            cur_time: DateTime32::from(cur_time),
            min_time: DateTime32::from(PREV - 100),
            max_time: DateTime32::from(PREV + BLOCK_MAX_TIME_SINCE_MEDIAN),
        }
    }

    /// A candidate time inside the old "eager" window (`PREV + 300 .. PREV + 450`) must stay
    /// standard-difficulty and must not be future-dated. Zebra used to switch this to a
    /// minimum-difficulty template with `cur_time` clamped up to `PREV + 451`.
    #[test]
    fn eager_window_stays_standard_difficulty_and_is_not_future_dated() {
        let network = Network::new_default_testnet();
        let cur = PREV + 400;
        let mut result = chain_info(cur);

        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            ACTIVE_HEIGHT,
            recent_block_data(&network),
        );

        // Still standard difficulty: expected_difficulty left untouched (sentinel unchanged).
        assert_eq!(result.expected_difficulty, CompactDifficulty::default());
        // Not future-dated: cur_time unchanged, still before the consensus threshold.
        assert_eq!(result.cur_time, DateTime32::from(cur));
        // max_time clamped down to the standard-difficulty boundary (PREV + 450).
        assert_eq!(result.max_time, DateTime32::from(PREV + GAP));
    }

    /// A candidate time strictly past `PREV + 450` switches to a minimum-difficulty template.
    #[test]
    fn past_the_threshold_switches_to_minimum_difficulty() {
        let network = Network::new_default_testnet();
        let cur = PREV + 500;
        let mut result = chain_info(cur);

        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            ACTIVE_HEIGHT,
            recent_block_data(&network),
        );

        // Minimum difficulty: expected_difficulty recomputed to the PoWLimit.
        assert_eq!(
            result.expected_difficulty,
            network.target_difficulty_limit().to_compact()
        );
        // min_time raised to PREV + 451; cur_time is already past it, so it is unchanged.
        assert_eq!(result.min_time, DateTime32::from(PREV + GAP + 1));
        assert_eq!(result.cur_time, DateTime32::from(cur));
    }

    /// The gap is a strict `>`: exactly `PREV + 450` is standard, `PREV + 451` is minimum.
    #[test]
    fn threshold_is_inclusive_for_standard_difficulty() {
        let network = Network::new_default_testnet();

        let mut at_threshold = chain_info(PREV + GAP);
        adjust_difficulty_and_time_for_testnet(
            &mut at_threshold,
            &network,
            ACTIVE_HEIGHT,
            recent_block_data(&network),
        );
        assert_eq!(at_threshold.expected_difficulty, CompactDifficulty::default());

        let mut past_threshold = chain_info(PREV + GAP + 1);
        adjust_difficulty_and_time_for_testnet(
            &mut past_threshold,
            &network,
            ACTIVE_HEIGHT,
            recent_block_data(&network),
        );
        assert_eq!(
            past_threshold.expected_difficulty,
            network.target_difficulty_limit().to_compact()
        );
    }

    /// The adjustment only applies to test networks: on Mainnet it is a no-op.
    #[test]
    fn mainnet_is_a_no_op() {
        let network = Network::Mainnet;
        let cur = PREV + 400;
        let mut result = chain_info(cur);

        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            ACTIVE_HEIGHT,
            recent_block_data(&network),
        );

        assert_eq!(result.expected_difficulty, CompactDifficulty::default());
        assert_eq!(result.cur_time, DateTime32::from(cur));
        assert_eq!(result.min_time, DateTime32::from(PREV - 100));
        assert_eq!(result.max_time, DateTime32::from(PREV + BLOCK_MAX_TIME_SINCE_MEDIAN));
    }

    /// Below `TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT` (299188) the rule is inactive, so the
    /// adjustment is a no-op even on Testnet.
    #[test]
    fn below_start_height_is_a_no_op() {
        let network = Network::new_default_testnet();
        let cur = PREV + 400;
        let mut result = chain_info(cur);

        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            Height(100_000),
            recent_block_data(&network),
        );

        assert_eq!(result.expected_difficulty, CompactDifficulty::default());
        assert_eq!(result.cur_time, DateTime32::from(cur));
        assert_eq!(result.max_time, DateTime32::from(PREV + BLOCK_MAX_TIME_SINCE_MEDIAN));
    }
}
