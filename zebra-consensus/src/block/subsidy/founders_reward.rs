//! Founders’ Reward calculations. - [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use num_integer::div_ceil;

use std::convert::TryFrom;
use std::str::FromStr;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
    transaction::Transaction,
    transparent::{Address, Output, ScriptForNetwork},
};

use crate::block::subsidy::general::{block_subsidy, halving_divisor};
use crate::parameters::subsidy::*;

/// `FoundersReward(height)` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn founders_reward(height: Height, network: Network) -> Result<Amount<NonNegative>, Error> {
    let canopy_height = Canopy
        .activation_height(network)
        .expect("canopy activation height should be available");
    if halving_divisor(height, network) == 1 && height < canopy_height {
        // this calculation is exact, because the block subsidy is divisible by
        // the FOUNDERS_FRACTION_DIVISOR until long after the first halving
        block_subsidy(height, network)? / FOUNDERS_FRACTION_DIVISOR
    } else {
        Amount::try_from(0)
    }
}

/// Function `FounderAddressChangeInterval` as specified in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward
pub fn founders_address_change_interval() -> Height {
    let interval = div_ceil(
        SLOW_START_SHIFT.0 + PRE_BLOSSOM_HALVING_INTERVAL.0,
        FOUNDERS_ADDRESS_COUNT,
    );
    Height(interval)
}

/// Get the founders reward t-address for the specified block height as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward
pub fn founders_reward_address(height: Height, network: Network) -> Result<Address, Error> {
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");
    let canopy_height = Canopy
        .activation_height(network)
        .expect("canopy activation height should be available");

    if height >= canopy_height {
        panic!("no address returned after canopy");
    }

    let mut adjusted_height = height;
    if height >= blossom_height {
        adjusted_height = Height(
            blossom_height.0
                + ((height.0 - blossom_height.0) / (BLOSSOM_POW_TARGET_SPACING_RATIO as u32)),
        );
    }

    let address_index = 1 + (adjusted_height.0 / founders_address_change_interval().0);

    let mut addresses = FOUNDERS_REWARD_ADDRESSES_MAINNET;
    if network == Network::Testnet {
        addresses = FOUNDERS_REWARD_ADDRESSES_TESTNET;
    }

    let address: Address = Address::from_str(addresses[(address_index - 1) as usize])
        .expect("we should get a taddress here");
    Ok(address)
}

/// Returns a list of outputs in `Transaction`, which have a script address equal to `String`.
pub fn find_output_with_address(
    transaction: &Transaction,
    address: Address,
    network: Network,
) -> Vec<Output> {
    transaction
        .outputs()
        .iter()
        .filter(|o| {
            o.lock_script.clone().to_address(network) == address
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;

    #[test]
    fn test_founders_reward() -> Result<(), Report> {
        zebra_test::init();

        founders_reward_for_network(Network::Mainnet)?;
        founders_reward_for_network(Network::Testnet)?;

        Ok(())
    }

    fn founders_reward_for_network(network: Network) -> Result<(), Report> {
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

    #[test]
    fn test_founders_address() -> Result<(), Report> {
        founders_address_for_network(Network::Mainnet)?;
        founders_address_for_network(Network::Testnet)?;

        Ok(())
    }

    fn founders_address_for_network(network: Network) -> Result<(), Report> {
        // First address change after blossom for the 2 networks.
        // Todo: explain this better and make sure it is working propertly.
        // after blossom there is still time left for the next change in
        // founder address but the remaining is calculated with the new formula.
        let mut addresses = FOUNDERS_REWARD_ADDRESSES_MAINNET;
        let mut first_change_after_blossom = 656_866;

        if network == Network::Testnet {
            addresses = FOUNDERS_REWARD_ADDRESSES_TESTNET;
            first_change_after_blossom = 584_794;
        }

        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        // the index in the founders reward address array that will be active at height
        let mut index = 0;

        // from genesis to blossom the founder reward address changes at FOUNDER_ADDRESS_CHANGE_INTERVAL
        for n in (1..blossom_height.0).step_by(founders_address_change_interval().0 as usize) {
            assert_eq!(
                founders_reward_address(Height(n), network)?,
                Address::from_str(addresses[index as usize]).expect("an address")
            );
            index += 1;
        }

        assert_eq!(
            founders_reward_address(Height(first_change_after_blossom), network)?,
            Address::from_str(addresses[index as usize]).expect("an address")
        );

        // after the first change after blossom the addresses changes at FOUNDER_ADDRESS_CHANGE_INTERVAL * 2
        for n in (first_change_after_blossom..canopy_height.0)
            .step_by((founders_address_change_interval().0 * 2) as usize)
        {
            assert_eq!(
                founders_reward_address(Height(n), network)?,
                Address::from_str(addresses[index as usize]).expect("an address")
            );
            index += 1;
        }

        Ok(())
    }

    #[test]
    fn test_founders_address_count() -> Result<(), Report> {
        assert_eq!(
            FOUNDERS_REWARD_ADDRESSES_MAINNET.len() as u32,
            FOUNDERS_ADDRESS_COUNT
        );
        assert_eq!(
            FOUNDERS_REWARD_ADDRESSES_TESTNET.len() as u32,
            FOUNDERS_ADDRESS_COUNT
        );

        Ok(())
    }

    #[test]
    fn test_founders_address_ceiling() -> Result<(), Report> {
        // Test proves why `div_ceil` needs to be used.
        // `truncate(n/d) + 1` is 1 more than `ceiling(n/d)` when `n/d` is an integer.
        // Suppose `SLOW_START_SHIFT.0 + PRE_BLOSSOM_HALVING_INTERVAL.0 = 480_000` instead
        // of current protocol defined value of `850_000`.
        let numerator = 480_000;

        // `truncate(n) + 1` will output the wrong result
        assert_eq!((numerator / FOUNDERS_ADDRESS_COUNT) + 1, 10001);

        // `div_ceil` will output the right thing
        assert_eq!(div_ceil(numerator, FOUNDERS_ADDRESS_COUNT), 10000);

        Ok(())
    }
}
