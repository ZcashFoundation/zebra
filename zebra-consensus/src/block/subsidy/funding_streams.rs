//! Funding Streams calculations. - [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::convert::TryFrom;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
};

use crate::{
    block::subsidy::general::block_subsidy,
    parameters::subsidy::{
        FundingStreamReceiver, FUNDING_STREAM_HEIGHT_RANGES, FUNDING_STREAM_RECEIVER_DENOMINATOR,
        FUNDING_STREAM_RECEIVER_NUMERATORS,
    },
};

#[cfg(test)]
mod tests;

/// Returns the `fs.Value(height)` for each stream receiver
/// as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
use std::collections::HashMap;
pub fn funding_stream_values(
    height: Height,
    network: Network,
) -> Result<HashMap<FundingStreamReceiver, Amount<NonNegative>>, Error> {
    let canopy_height = Canopy.activation_height(network).unwrap();
    let mut results = HashMap::new();

    if height >= canopy_height {
        let range = FUNDING_STREAM_HEIGHT_RANGES.get(&network).unwrap();
        if range.contains(&height) {
            let block_subsidy = block_subsidy(height, network)?;
            for (&receiver, &numerator) in FUNDING_STREAM_RECEIVER_NUMERATORS.iter() {
                let amount_value =
                    ((block_subsidy * numerator)? / FUNDING_STREAM_RECEIVER_DENOMINATOR)?;

                results.insert(receiver, amount_value);
            }
        }
    }
    Ok(results)
}
