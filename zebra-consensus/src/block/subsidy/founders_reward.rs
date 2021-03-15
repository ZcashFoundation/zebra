//! Founders' Reward calculations. - [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::convert::TryFrom;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::Network,
};

use crate::block::subsidy::general::{block_subsidy, halving_divisor};
use crate::parameters::subsidy::FOUNDERS_FRACTION_DIVISOR;

/// `FoundersReward(height)` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn founders_reward(height: Height, network: Network) -> Result<Amount<NonNegative>, Error> {
    if halving_divisor(height, network) == 1 {
        // this calculation is exact, because the block subsidy is divisible by
        // the FOUNDERS_FRACTION_DIVISOR until long after the first halving
        block_subsidy(height, network)? / FOUNDERS_FRACTION_DIVISOR
    } else {
        Amount::try_from(0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;
    use zebra_chain::parameters::NetworkUpgrade::*;
    #[test]
    fn test_founders_reward() -> Result<(), Report> {
        zebra_test::init();

        let network = Network::Mainnet;
        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        // Founders reward is 20% of the block subsidy
        // https://z.cash/support/faq/#founders-reward-recipients
        // Before Blossom this is 20*12.5/100 = 2.5 ZEC
        assert_eq!(
            Amount::try_from(250_000_000),
            founders_reward((blossom_height - 1).unwrap(), network)
        );
        // Founders reward is still 20% of the block subsidy but the block reward is half what it was
        // After Blossom this is 20*6.25/100 = 1.25 ZEC
        // https://z.cash/upgrade/blossom/
        assert_eq!(
            Amount::try_from(125_000_000),
            founders_reward(blossom_height, network)
        );

        // After first halving(coinciding with Canopy) founders reward will expire
        // https://z.cash/support/faq/#does-the-founders-reward-expire
        assert_eq!(Amount::try_from(0), founders_reward(canopy_height, network));

        Ok(())
    }
}
