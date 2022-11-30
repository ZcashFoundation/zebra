//! Get context and calculate difficulty for the next block.

use std::{borrow::Borrow, sync::Arc};

use chrono::{DateTime, Duration, TimeZone, Utc};

use zebra_chain::{
    block::{Block, Hash, Height},
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade, POW_AVERAGING_WINDOW},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty},
};

use crate::{
    service::{
        any_ancestor_blocks,
        check::{
            difficulty::{BLOCK_MAX_TIME_SINCE_MEDIAN, POW_MEDIAN_BLOCK_SPAN},
            AdjustedDifficulty,
        },
        finalized_state::ZebraDb,
        read::tree::history_tree,
        NonFinalizedState,
    },
    GetBlockTemplateChainInfo, HashOrHeight,
};

/// Returns :
/// - The `CompactDifficulty`, for the current best chain.
/// - The current system time.
/// - The minimum time for a next block.
/// - The maximum time for a next block.
/// - The history tree for the current best chain.
///
/// Panic if we don't have enough blocks in the state.
pub fn get_block_template_chain_info(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    tip: (Height, Hash),
    network: Network,
) -> GetBlockTemplateChainInfo {
    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, tip.1);
    let history_tree = history_tree(
        non_finalized_state.best_chain(),
        db,
        HashOrHeight::Hash(tip.1),
    )
    .expect("Hash passed should exist in the chain");

    difficulty_time_and_history_tree(relevant_chain, tip, network, history_tree)
}

fn difficulty_time_and_history_tree<C>(
    relevant_chain: C,
    tip: (Height, Hash),
    network: Network,
    history_tree: Arc<HistoryTree>,
) -> GetBlockTemplateChainInfo
where
    C: IntoIterator,
    C::Item: Borrow<Block>,
    C::IntoIter: ExactSizeIterator,
{
    const MAX_CONTEXT_BLOCKS: usize = POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN;

    let relevant_chain: Vec<_> = relevant_chain
        .into_iter()
        .take(MAX_CONTEXT_BLOCKS)
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
    assert!(relevant_data.len() < MAX_CONTEXT_BLOCKS);

    let current_system_time = chrono::Utc::now();

    // Get the median-time-past, which doesn't depend on the current system time.
    //
    // TODO: split out median-time-past into its own struct?
    let median_time_past = AdjustedDifficulty::new_from_header_time(
        current_system_time,
        tip.0,
        network,
        relevant_data.clone(),
    )
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
    let max_time = median_time_past
        .checked_add_signed(Duration::seconds(BLOCK_MAX_TIME_SINCE_MEDIAN))
        .expect("median time plus a small constant is far below i64::MAX");

    let current_system_time = current_system_time
        .timestamp()
        .clamp(min_time.timestamp(), max_time.timestamp());

    let mut current_system_time = Utc.timestamp_opt(current_system_time, 0).single().expect(
        "clamping a timestamp between two valid times can't make it invalid, and \
             UTC never has ambiguous time zone conversions",
    );

    // Now that we have a valid time, get the difficulty for that time.
    let mut difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        current_system_time,
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
    // In this case, we adjust the min_time and cur_time to the first minimum difficulty time.
    //
    // In rare cases, this could make some testnet miners produce invalid blocks,
    // if they use the full 90 minute time gap in the consensus rules.
    // (The getblocktemplate RPC reference doesn't have a max_time field,
    // so there is no standard way of telling miners that the max_time is smaller.)
    //
    // But that's better than obscure failures caused by changing the time a small amount,
    // if that moves the block from standard to minimum difficulty.
    if network == Network::Testnet {
        let max_time_difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
            max_time,
            tip.0,
            network,
            relevant_data.iter().cloned(),
        );

        // The max time is a minimum difficulty block,
        // so the time range could have different difficulties.
        if max_time_difficulty_adjustment.expected_difficulty_threshold()
            == ExpandedDifficulty::target_difficulty_limit(Network::Testnet).to_compact()
        {
            let min_time_difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
                min_time,
                tip.0,
                network,
                relevant_data.iter().cloned(),
            );

            // Part of the valid range has a different difficulty.
            // So we need to find the minimum time that is also a minimum difficulty block.
            // This is the valid range for miners.
            if min_time_difficulty_adjustment.expected_difficulty_threshold()
                != max_time_difficulty_adjustment.expected_difficulty_threshold()
            {
                let preceding_block_time = relevant_data.last().expect("has at least one block").1;
                let minimum_difficulty_spacing =
                    NetworkUpgrade::minimum_difficulty_spacing_for_height(network, tip.0)
                        .expect("just checked the minimum difficulty rule is active");

                // The first minimum difficulty time is strictly greater than the spacing.
                min_time = preceding_block_time + minimum_difficulty_spacing + Duration::seconds(1);

                // Update the difficulty and times to match
                if current_system_time < min_time {
                    current_system_time = min_time;
                }

                difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
                    current_system_time,
                    tip.0,
                    network,
                    relevant_data,
                );
            }
        }
    }

    GetBlockTemplateChainInfo {
        tip,
        expected_difficulty: difficulty_adjustment.expected_difficulty_threshold(),
        min_time,
        current_system_time,
        max_time,
        history_tree,
    }
}
