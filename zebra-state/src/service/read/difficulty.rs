//! Get context and calculate difficulty for the next block.

use std::borrow::Borrow;

use chrono::{DateTime, Duration, TimeZone, Utc};

use zebra_chain::{
    block::{Block, Hash, Height},
    parameters::{Network, NetworkUpgrade, POST_BLOSSOM_POW_TARGET_SPACING},
    work::difficulty::CompactDifficulty,
};

use crate::{
    service::{
        any_ancestor_blocks,
        check::{
            difficulty::{BLOCK_MAX_TIME_SINCE_MEDIAN, POW_ADJUSTMENT_BLOCK_SPAN},
            AdjustedDifficulty,
        },
        finalized_state::ZebraDb,
        NonFinalizedState,
    },
    GetBlockTemplateChainInfo,
};

/// Returns :
/// - The `CompactDifficulty`, for the current best chain.
/// - The current system time.
/// - The minimum time for a next block.
///
/// Panic if we don't have enough blocks in the state.
pub fn difficulty_and_time_info(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    tip: (Height, Hash),
    network: Network,
) -> GetBlockTemplateChainInfo {
    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, tip.1);
    difficulty_and_time(relevant_chain, tip, network)
}

fn difficulty_and_time<C>(
    relevant_chain: C,
    tip: (Height, Hash),
    network: Network,
) -> GetBlockTemplateChainInfo
where
    C: IntoIterator,
    C::Item: Borrow<Block>,
    C::IntoIter: ExactSizeIterator,
{
    let relevant_chain: Vec<_> = relevant_chain
        .into_iter()
        .take(POW_ADJUSTMENT_BLOCK_SPAN)
        .collect();

    let relevant_data: Vec<(CompactDifficulty, DateTime<Utc>)> = relevant_chain
        .iter()
        .map(|block| {
            (
                block.borrow().header.difficulty_threshold,
                block.borrow().header.time,
            )
        })
        .collect();

    // The getblocktemplate RPC returns an error if Zebra is not synced to the tip.
    // So this will never happen in production code.
    assert_eq!(
        relevant_data.len(),
        POW_ADJUSTMENT_BLOCK_SPAN,
        "getblocktemplate RPC called with empty state: should have returned an error",
    );

    let cur_time = chrono::Utc::now();

    // Get the median-time-past, which doesn't depend on the time or the previous block height.
    //
    // TODO: split out median-time-past into its own struct?
    let median_time_past =
        AdjustedDifficulty::new_from_header_time(cur_time, tip.0, network, relevant_data.clone())
            .median_time_past();

    // > For each block other than the genesis block , nTime MUST be strictly greater than
    // > the median-time-past of that block.
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let mut min_time = median_time_past
        .checked_add_signed(Duration::seconds(1))
        .expect("median time plus a small constant is far below i64::MAX");

    // > For each block at block height 2 or greater on Mainnet, or block height 653606 or greater on Testnet, nTime
    // > MUST be less than or equal to the median-time-past of that block plus 90 * 60 seconds.
    //
    // We ignore the height as we are checkpointing on Canopy or higher in Mainnet and Testnet.
    let mut max_time = median_time_past
        .checked_add_signed(Duration::seconds(BLOCK_MAX_TIME_SINCE_MEDIAN))
        .expect("median time plus a small constant is far below i64::MAX");

    let cur_time = cur_time
        .timestamp()
        .clamp(min_time.timestamp(), max_time.timestamp());

    let mut cur_time = Utc.timestamp_opt(cur_time, 0).single().expect(
        "clamping a timestamp between two valid times can't make it invalid, and \
             UTC never has ambiguous time zone conversions",
    );

    // Now that we have a valid time, get the difficulty for that time.
    let mut difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        cur_time,
        tip.0,
        network,
        relevant_data.iter().cloned(),
    );

    // On testnet, changing the block time can also change the difficulty,
    // due to the minimum difficulty consensus rule:
    // > if the block time of a block at height height ≥ 299188
    // > is greater than 6 * PoWTargetSpacing(height) seconds after that of the preceding block,
    // > then the block is a minimum-difficulty block.
    //
    // The max time is always a minimum difficulty block, because the minimum difficulty
    // gap is 15 minutes, but the maximum gap is 90 minutes. This means that testnet blocks
    // have two valid time ranges with different difficulties:
    // * 1s - 15m: standard difficulty
    // * 15m1s - 90m: minimum difficulty
    //
    // In rare cases, this could make some testnet miners produce invalid blocks,
    // if they use the full 90 minute time gap in the consensus rules.
    // (The zcashd getblocktemplate RPC reference doesn't have a max_time field,
    // so there is no standard way of telling miners that the max_time is smaller.)
    //
    // So Zebra adjusts the min or max times to produce a valid time range for the difficulty.
    // There is still a small chance that miners will produce an invalid block, if they are
    // just below the max time, and don't check it.
    if network == Network::Testnet {
        let preceding_block_time = relevant_data.last().expect("has at least one block").1;
        let minimum_difficulty_spacing =
            NetworkUpgrade::minimum_difficulty_spacing_for_height(network, tip.0)
                .expect("just checked testnet, and the RPC returns an error for low heights");

        // The first minimum difficulty time is strictly greater than the spacing.
        let std_difficulty_max_time = preceding_block_time + minimum_difficulty_spacing;
        let min_difficulty_min_time = std_difficulty_max_time + Duration::seconds(1);

        // If a miner is likely to find a block with the cur_time and standard difficulty
        //
        // We don't need to undo the clamping here:
        // - if cur_time is clamped to min_time, then we're more likely to have a minimum
        //    difficulty block, which makes mining easier;
        // - if cur_time gets clamped to max_time, this is already a minimum difficulty block.
        if cur_time + Duration::seconds(POST_BLOSSOM_POW_TARGET_SPACING * 2)
            <= std_difficulty_max_time
        {
            // Standard difficulty: the max time needs to exclude min difficulty blocks
            max_time = std_difficulty_max_time;
        } else {
            // Minimum difficulty: the min and cur time need to exclude min difficulty blocks
            min_time = min_difficulty_min_time;
            if cur_time < min_difficulty_min_time {
                cur_time = min_difficulty_min_time;
            }
            difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
                cur_time,
                tip.0,
                network,
                relevant_data.iter().cloned(),
            );
        }
    }

    GetBlockTemplateChainInfo {
        tip,
        expected_difficulty: difficulty_adjustment.expected_difficulty_threshold(),
        min_time,
        cur_time,
        max_time,
    }
}
