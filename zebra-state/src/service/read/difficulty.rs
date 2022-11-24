//! Get context and calculate difficulty for the next block.

use chrono::{DateTime, Duration, Utc};
use std::borrow::Borrow;

use zebra_chain::{
    block::{Block, Hash, Height},
    parameters::{Network, POW_AVERAGING_WINDOW},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty},
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

    let mut relevant_data = relevant_chain
        .iter()
        .map(|block| {
            (
                block.borrow().header.difficulty_threshold,
                block.borrow().header.time,
            )
        })
        .peekable();

    // get the tip time from the relevant chain
    let tip_time = relevant_data.peek().expect("tip should be here").1;

    // The getblocktemplate RPC returns an error if Zebra is not synced to the tip.
    // So this will never happen in production code.
    assert!(relevant_data.len() < MAX_CONTEXT_BLOCKS);

    let difficulty_adjustment = AdjustedDifficulty::new_from_header_time(
        *current_system_time,
        tip_height,
        network,
        relevant_data,
    );

    let compact_difficulty = match network {
        Network::Testnet => {
            // > if the block time of a block at height height ≥ 299188 is greater than 6 * PoWTargetSpacing(height) seconds after
            // > that of the preceding block, then the block is a minimum-difficulty block.
            // > In that case its nBits field MUST be set to ToCompact(PoWLimit)
            //
            // https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-testnet
            //
            // As we checkpoint on Canopy or higher in the testnet, we can:
            // - Ignore the height and,
            // - Use the constant` PostBlossomPoWTargetSpacing` spec value which is equal to 75 seconds.
            //
            // https://zips.z.cash/protocol/protocol.pdf#constants
            let pow_target_spacing_mul = Duration::seconds(6 * 75);

            if current_system_time
                > &tip_time
                    .checked_add_signed(pow_target_spacing_mul)
                    .expect("tip time plus a small constant is far below i64::MAX")
            {
                // We are in a minimum-difficulty block.
                ExpandedDifficulty::target_difficulty_limit(Network::Testnet).to_compact()
            } else {
                difficulty_adjustment.expected_difficulty_threshold()
            }
        }
        Network::Mainnet => difficulty_adjustment.expected_difficulty_threshold(),
    };

    (compact_difficulty, difficulty_adjustment.median_time_past())
}
