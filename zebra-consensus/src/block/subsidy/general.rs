//! Block and Miner subsidies, halvings and target spacing modifiers. - [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::convert::TryFrom;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
};

use crate::parameters::subsidy::*;

/// `SlowStartShift()` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
fn slow_start_shift() -> Height {
    Height(SLOW_START_INTERVAL.0 / 2)
}

/// The divisor used for halvings.
///
/// `1 << Halving(height)`, as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn halving_divisor(height: Height, network: Network) -> u64 {
    if height < SLOW_START_INTERVAL {
        panic!("can't verify before block {}", SLOW_START_INTERVAL.0)
    }
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");
    if height >= blossom_height {
        let scaled_pre_blossom_height =
            (blossom_height - slow_start_shift()) as u64 * BLOSSOM_POW_TARGET_SPACING_RATIO;
        let post_blossom_height = (height - blossom_height) as u64;
        let halving_shift = (scaled_pre_blossom_height + post_blossom_height)
            / (POST_BLOSSOM_HALVING_INTERVAL.0 as u64);
        1 << halving_shift
    } else {
        let scaled_pre_blossom_height = (height - slow_start_shift()) as u64;
        let halving_shift = scaled_pre_blossom_height / (PRE_BLOSSOM_HALVING_INTERVAL.0 as u64);
        1 << halving_shift
    }
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn block_subsidy(height: Height, network: Network) -> Result<Amount<NonNegative>, Error> {
    if height < SLOW_START_INTERVAL {
        panic!("can't verify before block {}", SLOW_START_INTERVAL.0)
    }
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    let hd = halving_divisor(height, network);
    if height >= blossom_height {
        let scaled_max_block_subsidy = MAX_BLOCK_SUBSIDY / BLOSSOM_POW_TARGET_SPACING_RATIO;
        // in future halvings, this calculation might not be exact
        // in those cases, Amount division follows integer division, which truncates (rounds down) the result
        Amount::try_from(scaled_max_block_subsidy / hd)
    } else {
        // this calculation is exact, because the halving divisor is 1 here
        Amount::try_from(MAX_BLOCK_SUBSIDY / hd)
    }
}

/// `MinerSubsidy(height)` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// `non_miner_reward` is the founders reward or funding stream value.
/// If all the rewards for a block go to the miner, use `None`.
#[allow(dead_code)]
pub fn miner_subsidy(
    height: Height,
    network: Network,
    non_miner_reward: Option<Amount<NonNegative>>,
) -> Result<Amount<NonNegative>, Error> {
    if let Some(non_miner_reward) = non_miner_reward {
        block_subsidy(height, network)? - non_miner_reward
    } else {
        block_subsidy(height, network)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;

    #[test]
    fn test_halving() -> Result<(), Report> {
        let network = Network::Mainnet;
        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        assert_eq!(1, halving_divisor((blossom_height - 1).unwrap(), network));
        assert_eq!(1, halving_divisor(blossom_height, network));
        assert_eq!(1, halving_divisor((canopy_height - 1).unwrap(), network));
        assert_eq!(2, halving_divisor(canopy_height, network));
        assert_eq!(2, halving_divisor((canopy_height + 1).unwrap(), network));

        Ok(())
    }

    #[test]
    fn test_block_subsidy() -> Result<(), Report> {
        let network = Network::Mainnet;
        let blossom_height = Blossom.activation_height(network).unwrap();
        let canopy_height = Canopy.activation_height(network).unwrap();

        // After slow-start mining and before Blossom the block reward is 12.5 ZEC
        // https://z.cash/support/faq/#what-is-slow-start-mining
        assert_eq!(
            Amount::try_from(1_250_000_000),
            block_subsidy((blossom_height - 1).unwrap(), network)
        );

        // After Blossom the block reward is reduced to 6.25 ZEC without halving
        // https://z.cash/upgrade/blossom/
        assert_eq!(
            Amount::try_from(625_000_000),
            block_subsidy(blossom_height, network)
        );

        // After 1st halving(coinciding with Canopy) the block reward is reduced to 3.125 ZEC
        // https://z.cash/upgrade/canopy/
        assert_eq!(
            Amount::try_from(312_500_000),
            block_subsidy(canopy_height, network)
        );

        Ok(())
    }

    #[test]
    fn miner_subsidy_test() -> Result<(), Report> {
        miner_subsidy_for_network(Network::Mainnet)?;
        miner_subsidy_for_network(Network::Testnet)?;

        Ok(())
    }

    fn miner_subsidy_for_network(network: Network) -> Result<(), Report> {
        use crate::block::subsidy::founders_reward::founders_reward;
        let blossom_height = Blossom.activation_height(network).unwrap();

        // Miner reward before Blossom is 80% of the total block reward
        // 80*12.5/100 = 10 ZEC
        let founders_amount = founders_reward((blossom_height - 1).unwrap(), network)?;
        assert_eq!(
            Amount::try_from(1_000_000_000),
            miner_subsidy(
                (blossom_height - 1).unwrap(),
                network,
                Some(founders_amount)
            )
        );

        // After blossom the total block reward is "halved", miner still get 80%
        // 80*6.25/100 = 5 ZEC
        let founders_amount = founders_reward(blossom_height, network)?;
        assert_eq!(
            Amount::try_from(500_000_000),
            miner_subsidy(blossom_height, network, Some(founders_amount))
        );

        // TODO: After first halving, miner will get 2.5 ZEC per mined block
        // but we need funding streams code to get this number

        // TODO: After second halving, there will be no funding streams, and
        // miners will get all the block reward

        Ok(())
    }

    #[test]
    fn subsidy_is_correct_test() -> Result<(), Report> {
        subsidy_is_correct(Network::Mainnet)?;
        subsidy_is_correct(Network::Testnet)?;

        Ok(())
    }
    fn subsidy_is_correct(network: Network) -> Result<(), Report> {
        use crate::block::check;
        use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};

        let block_iter = match network {
            Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
            Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
        };
        for (&height, block) in block_iter {
            let block = block
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid");

            if Height(height) > SLOW_START_INTERVAL
                && Height(height) < Canopy.activation_height(network).unwrap()
            {
                check::subsidy_is_correct(network, &block)
                    .expect("subsidies should pass for this block");
            }
        }
        Ok(())
    }
}
