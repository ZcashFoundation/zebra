//! Block validity checks

use super::Error;

use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;

use zebra_chain::block::Block;

/// Helper function for `node_time_check()`, see that function for details.
fn node_time_check_helper(
    block_header_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let two_hours_in_the_future = now
        .checked_add_signed(Duration::hours(2))
        .ok_or("overflow when calculating 2 hours in the future")?;

    if block_header_time <= two_hours_in_the_future {
        Ok(())
    } else {
        Err("block header time is more than 2 hours in the future".into())
    }
}

/// Check if the block header time is less than or equal to
/// 2 hours in the future, according to the node's local clock.
///
/// This is a non-deterministic rule, as clocks vary over time, and
/// between different nodes.
///
/// "In addition, a full validator MUST NOT accept blocks with nTime
/// more than two hours in the future according to its clock. This
/// is not strictly a consensus rule because it is nondeterministic,
/// and clock time varies between nodes. Also note that a block that
/// is rejected by this rule at a given point in time may later be
/// accepted."[S 7.5][7.5]
///
/// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
pub(super) fn node_time_check(block: Arc<Block>) -> Result<(), Error> {
    node_time_check_helper(block.header.time, Utc::now())
}
