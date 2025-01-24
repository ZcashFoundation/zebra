//! Block and Miner subsidies, halvings and target spacing modifiers. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

// TODO: Move the contents of this mod to the parent mod and remove this mod.

use std::collections::HashSet;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::{Height, HeightDiff},
    parameters::{subsidy::*, Network, NetworkUpgrade::*},
    transaction::Transaction,
};

use crate::{block::SubsidyError, funding_stream_values};

/// The divisor used for halvings.
///
/// `1 << Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// Returns `None` if the divisor would overflow a `u64`.
pub fn halving_divisor(height: Height, network: &Network) -> Option<u64> {
    // Some far-future shifts can be more than 63 bits
    1u64.checked_shl(num_halvings(height, network))
}

/// The halving index for a block height and network.
///
/// `Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn num_halvings(height: Height, network: &Network) -> u32 {
    let slow_start_shift = network.slow_start_shift();
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    let halving_index = if height < slow_start_shift {
        0
    } else if height < blossom_height {
        let pre_blossom_height = height - slow_start_shift;
        pre_blossom_height / network.pre_blossom_halving_interval()
    } else {
        let pre_blossom_height = blossom_height - slow_start_shift;
        let scaled_pre_blossom_height =
            pre_blossom_height * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO);

        let post_blossom_height = height - blossom_height;

        (scaled_pre_blossom_height + post_blossom_height) / network.post_blossom_halving_interval()
    };

    halving_index
        .try_into()
        .expect("already checked for negatives")
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn block_subsidy(
    height: Height,
    network: &Network,
) -> Result<Amount<NonNegative>, SubsidyError> {
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    // If the halving divisor is larger than u64::MAX, the block subsidy is zero,
    // because amounts fit in an i64.
    //
    // Note: bitcoind incorrectly wraps here, which restarts large block rewards.
    let Some(halving_div) = halving_divisor(height, network) else {
        return Ok(Amount::zero());
    };

    // Zebra doesn't need to calculate block subsidies for blocks with heights in the slow start
    // interval because it handles those blocks through checkpointing.
    if height < network.slow_start_interval() {
        Err(SubsidyError::UnsupportedHeight)
    } else if height < blossom_height {
        // this calculation is exact, because the halving divisor is 1 here
        Ok(Amount::try_from(MAX_BLOCK_SUBSIDY / halving_div)?)
    } else {
        let scaled_max_block_subsidy =
            MAX_BLOCK_SUBSIDY / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO);
        // in future halvings, this calculation might not be exact
        // Amount division is implemented using integer division,
        // which truncates (rounds down) the result, as specified
        Ok(Amount::try_from(scaled_max_block_subsidy / halving_div)?)
    }
}

/// `MinerSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn miner_subsidy(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, Error> {
    let total_funding_stream_amount: Result<Amount<NonNegative>, _> =
        funding_stream_values(height, network, expected_block_subsidy)?
            .values()
            .sum();

    expected_block_subsidy - total_funding_stream_amount?
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
        for network in Network::iter() {
            halving_for_network(&network)?;
        }

        Ok(())
    }

    fn halving_for_network(network: &Network) -> Result<(), Report> {
        let blossom_height = Blossom.activation_height(network).unwrap();
        let first_halving_height = network.height_for_first_halving();

        assert_eq!(
            1,
            halving_divisor((network.slow_start_interval() + 1).unwrap(), network).unwrap()
        );
        assert_eq!(
            1,
            halving_divisor((blossom_height - 1).unwrap(), network).unwrap()
        );
        assert_eq!(1, halving_divisor(blossom_height, network).unwrap());
        assert_eq!(
            1,
            halving_divisor((first_halving_height - 1).unwrap(), network).unwrap()
        );

        assert_eq!(2, halving_divisor(first_halving_height, network).unwrap());
        assert_eq!(
            2,
            halving_divisor((first_halving_height + 1).unwrap(), network).unwrap()
        );

        assert_eq!(
            4,
            halving_divisor(
                (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
                network
            )
            .unwrap()
        );
        assert_eq!(
            8,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 2)).unwrap(),
                network
            )
            .unwrap()
        );

        assert_eq!(
            1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 9)).unwrap(),
                network
            )
            .unwrap()
        );
        assert_eq!(
            1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 19)).unwrap(),
                network
            )
            .unwrap()
        );
        assert_eq!(
            1024 * 1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 29)).unwrap(),
                network
            )
            .unwrap()
        );
        assert_eq!(
            1024 * 1024 * 1024 * 1024,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 39)).unwrap(),
                network
            )
            .unwrap()
        );

        // The largest possible integer divisor
        assert_eq!(
            (i64::MAX as u64 + 1),
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 62)).unwrap(),
                network
            )
            .unwrap(),
        );

        // Very large divisors which should also result in zero amounts
        assert_eq!(
            None,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 63)).unwrap(),
                network,
            ),
        );

        assert_eq!(
            None,
            halving_divisor(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 64)).unwrap(),
                network,
            ),
        );

        assert_eq!(
            None,
            halving_divisor(Height(Height::MAX_AS_U32 / 4), network),
        );

        assert_eq!(
            None,
            halving_divisor(Height(Height::MAX_AS_U32 / 2), network),
        );

        assert_eq!(None, halving_divisor(Height::MAX, network));

        Ok(())
    }

    #[test]
    fn block_subsidy_test() -> Result<(), Report> {
        let _init_guard = zebra_test::init();

        for network in Network::iter() {
            block_subsidy_for_network(&network)?;
        }

        Ok(())
    }

    fn block_subsidy_for_network(network: &Network) -> Result<(), Report> {
        let blossom_height = Blossom.activation_height(network).unwrap();
        let first_halving_height = network.height_for_first_halving();

        // After slow-start mining and before Blossom the block subsidy is 12.5 ZEC
        // https://z.cash/support/faq/#what-is-slow-start-mining
        assert_eq!(
            Amount::<NonNegative>::try_from(1_250_000_000)?,
            block_subsidy((network.slow_start_interval() + 1).unwrap(), network)?
        );
        assert_eq!(
            Amount::<NonNegative>::try_from(1_250_000_000)?,
            block_subsidy((blossom_height - 1).unwrap(), network)?
        );

        // After Blossom the block subsidy is reduced to 6.25 ZEC without halving
        // https://z.cash/upgrade/blossom/
        assert_eq!(
            Amount::<NonNegative>::try_from(625_000_000)?,
            block_subsidy(blossom_height, network)?
        );

        // After the 1st halving, the block subsidy is reduced to 3.125 ZEC
        // https://z.cash/upgrade/canopy/
        assert_eq!(
            Amount::<NonNegative>::try_from(312_500_000)?,
            block_subsidy(first_halving_height, network)?
        );

        // After the 2nd halving, the block subsidy is reduced to 1.5625 ZEC
        // See "7.8 Calculation of Block Subsidy and Founders' Reward"
        assert_eq!(
            Amount::<NonNegative>::try_from(156_250_000)?,
            block_subsidy(
                (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
                network
            )?
        );

        // After the 7th halving, the block subsidy is reduced to 0.04882812 ZEC
        // Check that the block subsidy rounds down correctly, and there are no errors
        assert_eq!(
            Amount::<NonNegative>::try_from(4_882_812)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 6)).unwrap(),
                network
            )?
        );

        // After the 29th halving, the block subsidy is 1 zatoshi
        // Check that the block subsidy is calculated correctly at the limit
        assert_eq!(
            Amount::<NonNegative>::try_from(1)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 28)).unwrap(),
                network
            )?
        );

        // After the 30th halving, there is no block subsidy
        // Check that there are no errors
        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 29)).unwrap(),
                network
            )?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 39)).unwrap(),
                network
            )?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 49)).unwrap(),
                network
            )?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 59)).unwrap(),
                network
            )?
        );

        // The largest possible integer divisor
        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 62)).unwrap(),
                network
            )?
        );

        // Other large divisors which should also result in zero
        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 63)).unwrap(),
                network
            )?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(
                (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 64)).unwrap(),
                network
            )?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(Height(Height::MAX_AS_U32 / 4), network)?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(Height(Height::MAX_AS_U32 / 2), network)?
        );

        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(Height::MAX, network)?
        );

        Ok(())
    }

    #[test]
    fn check_height_for_num_halvings() {
        for network in Network::iter() {
            for halving in 1..1000 {
                let Some(height_for_halving) =
                    zebra_chain::parameters::subsidy::height_for_halving(halving, &network)
                else {
                    panic!("could not find height for halving {halving}");
                };

                let prev_height = height_for_halving
                    .previous()
                    .expect("there should be a previous height");

                assert_eq!(
                    halving,
                    num_halvings(height_for_halving, &network),
                    "num_halvings should match the halving index"
                );

                assert_eq!(
                    halving - 1,
                    num_halvings(prev_height, &network),
                    "num_halvings for the prev height should be 1 less than the halving index"
                );
            }
        }
    }
}
