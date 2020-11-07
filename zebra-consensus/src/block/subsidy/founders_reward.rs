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
    serialization::ZcashSerialize,
    transaction::Transaction,
    transparent::{Address, OpCode, Output, Script},
};

use crate::block::subsidy::general::{block_subsidy, halving_divisor};
use crate::parameters::subsidy::*;

/// Returns `true` if we are in the founders reward period of the blockchain.
pub fn founders_reward_active(height: Height, network: Network) -> bool {
    let canopy_activation_height = Canopy
        .activation_height(network)
        .expect("Canopy activation height is known");

    // The Zcash Specification and ZIPs explain the end of the founders reward in different ways,
    // because some were written before the set of Canopy network upgrade ZIPs was decided.
    // These are the canonical checks recommended by `zcashd` developers.
    height < canopy_activation_height && halving_divisor(height, network) == 1
}

/// `FoundersReward(height)` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn founders_reward(height: Height, network: Network) -> Result<Amount<NonNegative>, Error> {
    if founders_reward_active(height, network) {
        // this calculation is exact, because the block subsidy is divisible by
        // the FOUNDERS_FRACTION_DIVISOR until long after the first halving
        block_subsidy(height, network)? / FOUNDERS_FRACTION_DIVISOR
    } else {
        Amount::try_from(0)
    }
}

/// Function `FounderAddressChangeInterval` as specified in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#foundersreward
pub fn founders_address_change_interval() -> Height {
    let interval = div_ceil(
        SLOW_START_SHIFT.0 + PRE_BLOSSOM_HALVING_INTERVAL.0,
        FOUNDERS_ADDRESS_COUNT,
    );
    Height(interval)
}

/// Get the founders reward t-address for the specified block height as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#foundersreward
pub fn founders_reward_address(height: Height, network: Network) -> Result<Address, Error> {
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    if !founders_reward_active(height, network) {
        panic!("founders reward address lookup on invalid block: block is after canopy activation");
    }

    let mut adjusted_height = height;
    if height >= blossom_height {
        adjusted_height = Height(
            blossom_height.0
                + ((height.0 - blossom_height.0) / (BLOSSOM_POW_TARGET_SPACING_RATIO as u32)),
        );
    }

    let address_index = (adjusted_height.0 / founders_address_change_interval().0) as usize;
    let addresses = match network {
        Network::Mainnet => FOUNDERS_REWARD_ADDRESSES_MAINNET,
        Network::Testnet => FOUNDERS_REWARD_ADDRESSES_TESTNET,
    };
    let address: Address =
        Address::from_str(addresses[address_index]).expect("we should get a taddress here");

    Ok(address)
}
/// Given a founders reward address and a lock script from an output make sure the script
/// is well formed as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#foundersreward.
pub fn check_script_form(lock_script: Script, address: Address) -> bool {
    let mut address_hash = address
        .zcash_serialize_to_vec()
        .expect("we should get address bytes here");
    let mut lock_script_hash = lock_script.0;

    // Make sure the lock script haves the start and end we need.
    if !(lock_script_hash[0] == OpCode::Hash160 as u8
        && lock_script_hash[lock_script_hash.len() - 1] == OpCode::Equal as u8)
    {
        return false;
    }

    // Remove prefix from lock_script.
    let prefix_len = 2;
    lock_script_hash = address_hash[prefix_len..address_hash.len() - prefix_len].to_vec();

    // Remove prefix from given address.
    address_hash = address_hash[prefix_len..address_hash.len() - prefix_len].to_vec();

    // To be valid the bytes in the center of the lock_script hash from output should be the
    // same as the ones in the center of the given address hash.
    if lock_script_hash == address_hash {
        return true;
    }
    false
}

/// Returns a list of outputs in `Transaction`, which have a script address equal to `String`.
pub fn find_output_with_address(transaction: &Transaction, address: Address) -> Vec<Output> {
    transaction
        .outputs()
        .iter()
        .filter(|o| check_script_form(o.lock_script.clone(), address))
        .cloned()
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;

    #[test]
    fn test_founders_reward_active() -> Result<(), Report> {
        founders_reward_active_for_network(Network::Mainnet)?;
        founders_reward_active_for_network(Network::Testnet)?;

        Ok(())
    }

    fn founders_reward_active_for_network(network: Network) -> Result<(), Report> {
        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        assert_eq!(founders_reward_active(blossom_height, network), true);
        assert_eq!(founders_reward_active(canopy_height, network), false);

        Ok(())
    }

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

        // from SLOW_START_SHIFT to blossom the founder reward address changes at FOUNDER_ADDRESS_CHANGE_INTERVAL
        for n in (SLOW_START_SHIFT.0..blossom_height.0)
            .step_by(founders_address_change_interval().0 as usize)
        {
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
}
