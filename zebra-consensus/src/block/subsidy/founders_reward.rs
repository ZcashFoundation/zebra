//! Founders’ Reward calculations. - [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::convert::TryFrom;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
    transaction::Transaction,
    transparent::{Address, Output},
};

use crate::block::subsidy::general::{block_subsidy, halving_divisor};
use crate::parameters::subsidy::{
    BLOSSOM_POW_TARGET_SPACING_RATIO, FOUNDERS_FRACTION_DIVISOR, FOUNDERS_REWARD_ADDRESSES_MAINNET,
    FOUNDER_ADDRESS_CHANGE_INTERVAL,
};

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

/// Get the founders reward t-address for the specified block height as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/canopy.pdf#foundersreward
pub fn founders_reward_address(height: Height, network: Network) -> Result<String, Error> {
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

    let address_index = 1 + (adjusted_height.0 / FOUNDER_ADDRESS_CHANGE_INTERVAL as u32);
    Ok(FOUNDERS_REWARD_ADDRESSES_MAINNET[(address_index - 1) as usize].to_string())
}

/// Returns a list of outputs in `Transaction`, which have a script address equal to `String`.
pub fn find_output_with_address(transaction: &Transaction, address: &str) -> Vec<Output> {
    let calculated_addr: Address = address.parse().unwrap();

    // For debugging
    println!("Looking for founders reward address in new coinbase transaction:");
    for (output_number, o) in transaction.outputs().iter().enumerate() {
        let lock_script = o.lock_script.clone();

        // For one of the coinbase outputs the address we get from the lock_script should be one of
        // the ones in the FOUNDERS_REWARD_ADDRESSES_MAINNET array, not happening.
        let output_address: Address = Address::from(lock_script);

        println!(
            "Output: {} Calculated address: {} Output address: {}",
            output_number, calculated_addr, output_address
        );
        println!();
    }

    // never matching
    transaction
        .outputs()
        .iter()
        .filter(|o| Address::from(o.lock_script.clone()) == calculated_addr)
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

    #[test]
    fn test_founders_address() -> Result<(), Report> {
        let network = Network::Mainnet;

        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        // the index in the founders reward address array that will be active at height
        let mut index = 0;

        // from genesis to blossom the founder reward address changes at FOUNDER_ADDRESS_CHANGE_INTERVAL
        for n in (1..blossom_height.0).step_by(FOUNDER_ADDRESS_CHANGE_INTERVAL as usize) {
            assert_eq!(
                founders_reward_address(Height(n), network)?,
                FOUNDERS_REWARD_ADDRESSES_MAINNET[index as usize].to_string()
            );
            index += 1;
        }

        // after blossom the first change happening at block 656_866 in the mainnet
        // Todo: explain this better - after blossom there is still time left for the next change in
        // founder address but the remaining is calculated with the new formula.
        let first_change_after_blossom_mainnet = 656_866;
        assert_eq!(
            founders_reward_address(Height(first_change_after_blossom_mainnet), network)?,
            FOUNDERS_REWARD_ADDRESSES_MAINNET[index as usize].to_string()
        );

        // after the first change after blossom the addresses changes at FOUNDER_ADDRESS_CHANGE_INTERVAL * 2
        for n in (first_change_after_blossom_mainnet..canopy_height.0)
            .step_by((FOUNDER_ADDRESS_CHANGE_INTERVAL * 2) as usize)
        {
            assert_eq!(
                founders_reward_address(Height(n), network)?,
                FOUNDERS_REWARD_ADDRESSES_MAINNET[index as usize].to_string()
            );
            index += 1;
        }

        Ok(())
    }
}
