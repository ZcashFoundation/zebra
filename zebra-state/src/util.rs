use std::iter;
use zebra_chain::block;

use crate::constants;

/// Get the heights of the blocks for constructing a block_locator list
pub fn block_locator_heights(tip_height: block::Height) -> Vec<block::Height> {
    // Stop at the reorg limit, or the genesis block.
    let min_locator_height = tip_height
        .0
        .saturating_sub(constants::MAX_BLOCK_REORG_HEIGHT);
    let locators = iter::successors(Some(1u32), |h| h.checked_mul(2))
        .flat_map(move |step| tip_height.0.checked_sub(step));
    let locators = iter::once(tip_height.0)
        .chain(locators)
        .take_while(move |&height| height > min_locator_height)
        .chain(iter::once(min_locator_height))
        .map(block::Height);

    let locators = locators.collect();
    tracing::debug!(
        ?tip_height,
        ?min_locator_height,
        ?locators,
        "created block locator"
    );
    locators
}
