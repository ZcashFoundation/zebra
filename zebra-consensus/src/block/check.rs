//! Consensus check functions

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, Header},
    work::equihash,
};

use crate::error::*;
use crate::BoxError;

use zebra_chain::parameters::{Network, NetworkUpgrade::*};

use super::subsidy;

/// Check that there is exactly one coinbase transaction in `Block`, and that
/// the coinbase transaction is the first transaction in the block.
///
/// "The first (and only the first) transaction in a block is a coinbase
/// transaction, which collects and spends any miner subsidy and transaction
/// fees paid by transactions included in this block." [§3.10][3.10]
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub fn coinbase_is_first(block: &Block) -> Result<(), BlockError> {
    let first = block
        .transactions
        .get(0)
        .ok_or(BlockError::NoTransactions)?;
    let mut rest = block.transactions.iter().skip(1);
    if !first.is_coinbase() {
        return Err(TransactionError::CoinbasePosition)?;
    }
    if rest.any(|tx| tx.contains_coinbase_input()) {
        return Err(TransactionError::CoinbaseInputFound)?;
    }

    Ok(())
}

/// Returns true if the header is valid based on its `EquihashSolution`
pub fn equihash_solution_is_valid(header: &Header) -> Result<(), equihash::Error> {
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
pub fn time_is_valid_at(header: &Header, now: DateTime<Utc>) -> Result<(), BoxError> {
    header.is_time_valid_at(now)
}

/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_correct(network: Network, block: &Block) -> Result<(), BlockError> {
    let height = block
        .coinbase_height()
        .expect("always called on blocks with a coinbase height");

    let coinbase = block.transactions.get(0).ok_or(SubsidyError::NoCoinbase)?;
    let outputs = coinbase.outputs();

    let canopy_height = Canopy
        .activation_height(network)
        .ok_or(SubsidyError::NoCanopy)?;
    if height >= canopy_height {
        panic!("Can't validate Canopy yet");
    }

    // validate founders reward
    let mut valid_founders_reward = false;
    if height < canopy_height {
        let founders_reward = subsidy::founders_reward::founders_reward(height, network)
            .expect("founders reward should be always a valid value");

        let values = || outputs.iter().map(|o| o.value);

        if values().any(|value: Amount<NonNegative>| value == founders_reward) {
            valid_founders_reward = true;
        }
    }
    if !valid_founders_reward {
        Err(SubsidyError::FoundersRewardNotFound)?
    } else {
        // TODO: the exact founders reward value must be sent as a single output to the correct address
        // TODO: the sum of the coinbase transaction outputs must be less than or equal to the block subsidy plus transaction fees
        Ok(())
    }
}
