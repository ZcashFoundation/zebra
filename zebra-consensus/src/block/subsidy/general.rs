//! Block and Miner subsidies, halvings and target spacing modifiers. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::{collections::HashSet, convert::TryFrom};

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
    transaction::Transaction,
};

use crate::parameters::subsidy::*;

/// The divisor used for halvings.
///
/// `1 << Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn halving_divisor(height: Height, network: Network) -> u64 {
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    if height < SLOW_START_SHIFT {
        unreachable!(
            "unsupported block height: callers should handle blocks below {:?}",
            SLOW_START_SHIFT
        )
    } else if height < blossom_height {
        let scaled_pre_blossom_height = (height - SLOW_START_SHIFT) as u64;
        let halving_shift = scaled_pre_blossom_height / (PRE_BLOSSOM_HALVING_INTERVAL.0 as u64);
        1 << halving_shift
    } else {
        let scaled_pre_blossom_height =
            (blossom_height - SLOW_START_SHIFT) as u64 * BLOSSOM_POW_TARGET_SPACING_RATIO;
        let post_blossom_height = (height - blossom_height) as u64;
        let halving_shift = (scaled_pre_blossom_height + post_blossom_height)
            / (POST_BLOSSOM_HALVING_INTERVAL.0 as u64);
        1 << halving_shift
    }
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn block_subsidy(height: Height, network: Network) -> Result<Amount<NonNegative>, Error> {
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");
    let halving_div = halving_divisor(height, network);

    if height < SLOW_START_INTERVAL {
        unreachable!(
            "unsupported block height: callers should handle blocks below {:?}",
            SLOW_START_INTERVAL
        )
    } else if height < blossom_height {
        // this calculation is exact, because the halving divisor is 1 here
        Amount::try_from(MAX_BLOCK_SUBSIDY / halving_div)
    } else {
        let scaled_max_block_subsidy = MAX_BLOCK_SUBSIDY / BLOSSOM_POW_TARGET_SPACING_RATIO;
        // in future halvings, this calculation might not be exact
        // Amount division is implemented using integer division,
        // which truncates (rounds down) the result, as specified
        Amount::try_from(scaled_max_block_subsidy / halving_div)
    }
}

/// Returns all output amounts in `Transaction`.
pub fn output_amounts(transaction: &Transaction) -> HashSet<Amount<NonNegative>> {
    transaction
        .outputs()
        .iter()
        .map(|o| &o.value)
        .cloned()
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;

    #[test]
    fn halving_test() -> Result<(), Report> {
        let _init_guard = zebra_test::init();

        halving_for_network(Network::Mainnet)?;
        halving_for_network(Network::Testnet)?;

        Ok(())
    }

    fn halving_for_network(network: Network) -> Result<(), Report> {
        let blossom_height = Blossom.activation_height(network).unwrap();
        let first_halving_height = match network {
            Network::Mainnet => Canopy.activation_height(network).unwrap(),
            // Based on "7.8 Calculation of Block Subsidy and Founders' Reward"
            Network::Testnet => Height(1_116_000),
        };

        assert_eq!(1, halving_divisor((blossom_height - 1).unwrap(), network));
        assert_eq!(1, halving_divisor(blossom_height, network));
        assert_eq!(
            1,
            halving_divisor((first_halving_height - 1).unwrap(), network)
        );

        assert_eq!(2, halving_divisor(first_halving_height, network));
        assert_eq!(
            2,
            halving_divisor((first_halving_height + 1).unwrap(), network)
        );

        assert_eq!(
            4,
            halving_divisor(
                (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
                network
            )
        );
        assert_eq!(
            8,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 2)).unwrap(),
                network
            )
        );

        assert_eq!(
            1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 9)).unwrap(),
                network
            )
        );
        assert_eq!(
            1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 19)).unwrap(),
                network
            )
        );
        assert_eq!(
            1024 * 1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 29)).unwrap(),
                network
            )
        );
        assert_eq!(
            1024 * 1024 * 1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 39)).unwrap(),
                network
            )
        );

        // The largest possible divisor
        assert_eq!(
            1 << 63,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 62)).unwrap(),
                network
            )
        );

        Ok(())
    }

    #[test]
    fn block_subsidy_test() -> Result<(), Report> {
        let _init_guard = zebra_test::init();

        block_subsidy_for_network(Network::Mainnet)?;
        block_subsidy_for_network(Network::Testnet)?;

        Ok(())
    }

    fn block_subsidy_for_network(network: Network) -> Result<(), Report> {
        let blossom_height = Blossom.activation_height(network).unwrap();
        let first_halving_height = match network {
            Network::Mainnet => Canopy.activation_height(network).unwrap(),
            // Based on "7.8 Calculation of Block Subsidy and Founders' Reward"
            Network::Testnet => Height(1_116_000),
        };

        // After slow-start mining and before Blossom the block subsidy is 12.5 ZEC
        // https://z.cash/support/faq/#what-is-slow-start-mining
        assert_eq!(
            Amount::try_from(1_250_000_000),
            block_subsidy((blossom_height - 1).unwrap(), network)
        );

        // After Blossom the block subsidy is reduced to 6.25 ZEC without halving
        // https://z.cash/upgrade/blossom/
        assert_eq!(
            Amount::try_from(625_000_000),
            block_subsidy(blossom_height, network)
        );

        // After the 1st halving, the block subsidy is reduced to 3.125 ZEC
        // https://z.cash/upgrade/canopy/
        assert_eq!(
            Amount::try_from(312_500_000),
            block_subsidy(first_halving_height, network)
        );

        // After the 2nd halving, the block subsidy is reduced to 1.5625 ZEC
        // See "7.8 Calculation of Block Subsidy and Founders' Reward"
        assert_eq!(
            Amount::try_from(156_250_000),
            block_subsidy(
                (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
                network
            )
        );

        // After the 7th halving, the block subsidy is reduced to 0.04882812 ZEC
        // Check that the block subsidy rounds down correctly, and there are no errors
        assert_eq!(
            Amount::try_from(4_882_812),
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 6)).unwrap(),
                network
            )
        );

        // After the 29th halving, the block subsidy is 1 zatoshi
        // Check that the block subsidy is calculated correctly at the limit
        assert_eq!(
            Amount::try_from(1),
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 28)).unwrap(),
                network
            )
        );

        // After the 30th halving, there is no block subsidy
        // Check that there are no errors
        assert_eq!(
            Amount::try_from(0),
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 29)).unwrap(),
                network
            )
        );

        // The largest possible divisor
        assert_eq!(
            Amount::try_from(0),
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL.0 as i32 * 62)).unwrap(),
                network
            )
        );

        Ok(())
    }
}
