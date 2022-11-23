//! Get context and calculate difficulty for the next block.

use chrono::{DateTime, Utc};
use std::borrow::Borrow;

use zebra_chain::{
    block::{Block, Hash, Height},
    parameters::{Network, POW_AVERAGING_WINDOW},
    work::difficulty::CompactDifficulty,
};

use crate::service::{
    any_ancestor_blocks,
    check::{difficulty::POW_MEDIAN_BLOCK_SPAN, AdjustedDifficulty},
    finalized_state::ZebraDb,
    NonFinalizedState,
};

/// Return the `CompactDifficulty` and `median_past_time` for the current best chain.
///
/// Panic if we don't have enough blocks in the state.
pub fn adjusted_difficulty_data(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    tip: (Height, Hash),
    network: Network,
    current_system_time: &DateTime<Utc>,
) -> (CompactDifficulty, DateTime<Utc>) {
    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, tip.1);
    difficulty(relevant_chain, tip.0, network, current_system_time)
}

fn difficulty<C>(
    relevant_chain: C,
    tip_height: Height,
    network: Network,
    current_system_time: &DateTime<Utc>,
) -> (CompactDifficulty, chrono::DateTime<chrono::Utc>)
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

    let difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        *current_system_time,
        tip_height,
        network,
        relevant_data,
    );

    (
        difficulty_adjustment.expected_difficulty_threshold(),
        difficulty_adjustment.median_time_past(),
    )
}
