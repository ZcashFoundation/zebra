//! Get context and calculate difficulty for the next block.

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use std::borrow::Borrow;

use zebra_chain::{
    block::{Block, Hash, Height},
    parameters::{Network, POW_AVERAGING_WINDOW},
    work::difficulty::CompactDifficulty,
};

use crate::service::{
    any_ancestor_blocks,
    check::{
        difficulty::{BLOCK_MAX_TIME_SINCE_MEDIAN, POW_MEDIAN_BLOCK_SPAN},
        AdjustedDifficulty,
    },
    finalized_state::ZebraDb,
    NonFinalizedState,
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
) -> (CompactDifficulty, DateTime<Utc>, DateTime<Utc>) {
    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, tip.1);
    difficulty_and_time(relevant_chain, tip.0, network)
}

fn difficulty_and_time<C>(
    relevant_chain: C,
    tip_height: Height,
    network: Network,
) -> (CompactDifficulty, DateTime<Utc>, DateTime<Utc>)
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

    let relevant_data = relevant_chain.iter().map(|block| {
        (
            block.borrow().header.difficulty_threshold,
            block.borrow().header.time,
        )
    });

    // The getblocktemplate RPC returns an error if Zebra is not synced to the tip.
    // So this will never happen in production code.
    assert!(relevant_data.len() < MAX_CONTEXT_BLOCKS);

    let current_system_time = chrono::Utc::now();

    let difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        current_system_time,
        tip_height,
        network,
        relevant_data.clone(),
    );

    // > For each block other than the genesis block , nTime MUST be strictly greater than
    // > the median-time-past of that block.
    // https://zips.z.cash/protocol/protocol.pdf#blockheader
    let min_time = difficulty_adjustment
        .median_time_past()
        .checked_add_signed(Duration::seconds(1))
        .expect("median time plus a small constant is far below i64::MAX");

    // > For each block at block height 2 or greater on Mainnet, or block height 653606 or greater on Testnet, nTime
    // > MUST be less than or equal to the median-time-past of that block plus 90 * 60 seconds.
    //
    // We ignore the height as we are checkpointing on Canopy or higher in Mainnet and Testnet.
    let max_time = difficulty_adjustment
        .median_time_past()
        .checked_add_signed(Duration::seconds(BLOCK_MAX_TIME_SINCE_MEDIAN))
        .expect("median time plus a small constant is far below i64::MAX");

    let current_system_time = DateTime::<Utc>::from_utc(
        NaiveDateTime::from_timestamp_opt(
            current_system_time
                .timestamp()
                .clamp(min_time.timestamp(), max_time.timestamp()),
            0,
        )
        .expect("We should have an in range timestamp"),
        Utc,
    );

    let difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        current_system_time,
        tip_height,
        network,
        relevant_data,
    );

    (
        difficulty_adjustment.expected_difficulty_threshold(),
        current_system_time,
        min_time,
    )
}
