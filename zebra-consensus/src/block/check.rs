//! Consensus check functions

use super::*;
use chrono::{DateTime, Utc};
use zebra_chain::{
    block::{Block, Header},
    work::equihash,
};

/// Check that there is exactly one coinbase transaction in `Block`, and that
/// the coinbase transaction is the first transaction in the block.
///
/// "The first (and only the first) transaction in a block is a coinbase
/// transaction, which collects and spends any miner subsidy and transaction
/// fees paid by transactions included in this block." [§3.10][3.10]
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub fn is_coinbase_first(block: &Block) -> Result<(), Error> {
    let first = block
        .transactions
        .get(0)
        .ok_or("block has no transactions")?;
    let mut rest = block.transactions.iter().skip(1);
    if !first.is_coinbase() {
        return Err("first transaction must be coinbase".into());
    }
    if rest.any(|tx| tx.contains_coinbase_input()) {
        return Err("coinbase input found in non-coinbase transaction".into());
    }
    Ok(())
}

/// Returns true if the header is valid based on its `EquihashSolution`
pub fn is_equihash_solution_valid(header: &Header) -> Result<(), equihash::Error> {
    header.solution.check(&header)
}

/// Check if `header.time` is less than or equal to
/// 2 hours in the future, according to the node's local clock (`now`).
///
/// This is a non-deterministic rule, as clocks vary over time, and
/// between different nodes.
///
/// "In addition, a full validator MUST NOT accept blocks with nTime
/// more than two hours in the future according to its clock. This
/// is not strictly a consensus rule because it is nondeterministic,
/// and clock time varies between nodes. Also note that a block that
/// is rejected by this rule at a given point in time may later be
/// accepted." [§7.5][7.5]
///
/// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
pub fn is_time_valid_at(header: &Header, now: DateTime<Utc>) -> Result<(), Error> {
    header.is_time_valid_at(now)
}
