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
            for (&receiver, &numerator) in FUNDING_STREAM_RECEIVER_NUMERATORS.iter() {
                let amount_value = (block_subsidy(height, network)?.number() as f64
                    * (numerator as f64 / FUNDING_STREAM_RECEIVER_DENOMINATOR as f64))
                    .floor();
                results.insert(
                    receiver,
                    Amount::<NonNegative>::try_from(amount_value as i64)?,
                );
            }
            // Return the hashmap with the funding stream value for each receiver
            Ok(results)
        } else {
            // Return empty hashmap if we are out of the range
            Ok(results)
        }
    } else {
        // Return empty hashmap if we are before Canopy activation height
        Ok(results)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;
    #[test]
    // Check funding streams are correct in the entire period.
    fn test_funding_stream() -> Result<(), Report> {
        zebra_test::init();
        let network = Network::Mainnet;

        // funding streams not active
        let canopy_height_minus1 = Canopy.activation_height(network).unwrap() - 1;
        assert!(funding_stream_values(canopy_height_minus1.unwrap(), network)?.is_empty());

        // funding stream is active
        let canopy_height = Canopy.activation_height(network);
        let canopy_height_plus1 = Canopy.activation_height(network).unwrap() + 1;
        let canopy_height_plus2 = Canopy.activation_height(network).unwrap() + 2;

        let mut hash_map = HashMap::new();
        hash_map.insert(FundingStreamReceiver::Ecc, Amount::try_from(21_875_000)?);
        hash_map.insert(
            FundingStreamReceiver::ZcashFoundation,
            Amount::try_from(15_625_000)?,
        );
        hash_map.insert(
            FundingStreamReceiver::MajorGrants,
            Amount::try_from(25_000_000)?,
        );

        assert_eq!(
            funding_stream_values(canopy_height.unwrap(), network).unwrap(),
            hash_map
        );
        assert_eq!(
            funding_stream_values(canopy_height_plus1.unwrap(), network).unwrap(),
            hash_map
        );
        assert_eq!(
            funding_stream_values(canopy_height_plus2.unwrap(), network).unwrap(),
            hash_map
        );

        // funding stream period is ending
        let range = FUNDING_STREAM_HEIGHT_RANGES.get(&network).unwrap();
        let end = range.end;
        let last = end - 1;

        assert_eq!(
            funding_stream_values(last.unwrap(), network).unwrap(),
            hash_map
        );
        assert!(funding_stream_values(end, network)?.is_empty());

        Ok(())
    }
}
