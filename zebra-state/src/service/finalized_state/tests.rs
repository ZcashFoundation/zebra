//! Finalized state tests.

#![allow(clippy::unwrap_in_result)]

use zebra_chain::block::Height;

mod prop;
mod rollback;
mod transparent;
mod vectors;

#[test]
fn checkpoint_prune_range_retains_current_height_when_range_ends_before_it() {
    let current_height = Height(9);

    assert!(
        super::checkpoint_prune_range_retains_current_height(
            current_height,
            Some((Height(1), current_height)),
        ),
        "raw transactions are still needed when the prune range ends before the current height"
    );

    assert!(
        !super::checkpoint_prune_range_retains_current_height(
            current_height,
            Some((Height(1), Height(10))),
        ),
        "raw transactions can be skipped when the prune range covers the current height"
    );

    assert!(
        !super::checkpoint_prune_range_retains_current_height(current_height, None),
        "no checkpoint prune range means there is no archive backlog to drain"
    );
}
