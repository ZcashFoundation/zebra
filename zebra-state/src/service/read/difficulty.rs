//! Get context and calculate difficulty for the next block.
//!
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

/// Return the CompactDifficulty for the current best chain.
///
/// Note: Return `1` as the difficulty if we don't have enough blocks in the state. Should not happen in a
/// running blockchain but only in some test cases where not enough state is loaded.
pub fn relevant_chain_difficulty(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    tip: (Height, Hash),
    network: Network,
) -> Option<CompactDifficulty> {
    let relevant_chain = any_ancestor_blocks(non_finalized_state, db, tip.1);
    difficulty(relevant_chain, tip.0, network)
}

fn difficulty<C>(
    relevant_chain: C,
    tip_height: Height,
    network: Network,
) -> Option<CompactDifficulty>
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

    if relevant_data.len() < MAX_CONTEXT_BLOCKS {
        return None;
    }

    let time = chrono::Utc::now();
    let difficulty_adjustment =
        AdjustedDifficulty::new_from_header_time(time, tip_height, network, relevant_data);

    Some(difficulty_adjustment.expected_difficulty_threshold())
}
