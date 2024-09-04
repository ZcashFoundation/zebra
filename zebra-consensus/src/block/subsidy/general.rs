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
    let blossom_height = Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    if height < blossom_height {
        let pre_blossom_height = height - network.slow_start_shift();
        let halving_shift = pre_blossom_height / PRE_BLOSSOM_HALVING_INTERVAL;

        let halving_div = 1u64
            .checked_shl(
                halving_shift
                    .try_into()
                    .expect("already checked for negatives"),
            )
            .expect("pre-blossom heights produce small shifts");

        Some(halving_div)
    } else {
        let pre_blossom_height = blossom_height - network.slow_start_shift();
        let scaled_pre_blossom_height =
            pre_blossom_height * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO);

        let post_blossom_height = height - blossom_height;

        let halving_shift =
            (scaled_pre_blossom_height + post_blossom_height) / POST_BLOSSOM_HALVING_INTERVAL;

        // Some far-future shifts can be more than 63 bits
        1u64.checked_shl(
            halving_shift
                .try_into()
                .expect("already checked for negatives"),
        )
    }
}

#[cfg(feature = "zsf")]
pub fn block_subsidy(
    height: Height,
    network: &Network,
    zsf_balance: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, SubsidyError> {
    let zsf_activation_height = ZFuture
        .activation_height(network)
        .expect("ZFuture activation height should be available");

    if height < zsf_activation_height {
        block_subsidy_pre_zsf(height, network)
    } else {
        let zsf_balance: i64 = zsf_balance.into();
        let zsf_balance: i128 = zsf_balance.into();
        const BLOCK_SUBSIDY_DENOMINATOR: i128 = 10_000_000_000;
        const BLOCK_SUBSIDY_NUMERATOR: i128 = 4_126;

        // calculate the block subsidy (in zatoshi) using the ZSF balance, note the rounding up
        let subsidy = (zsf_balance * BLOCK_SUBSIDY_NUMERATOR + (BLOCK_SUBSIDY_DENOMINATOR - 1))
            / BLOCK_SUBSIDY_DENOMINATOR;

        Ok(subsidy.try_into().expect("subsidy should fit in Amount"))
    }
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn block_subsidy_pre_zsf(
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

/// Lockbox funding stream total input value for a block height.
///
/// Assumes a constant funding stream amount per block.
// TODO: Cache the lockbox value balance in zebra-state (will be required for tracking lockbox
//       value balance after the Zcash Sustainability Fund ZIPs or after a ZIP for spending from the deferred pool)
#[allow(dead_code)]
fn lockbox_input_value(network: &Network, height: Height) -> Amount<NonNegative> {
    let Some(nu6_activation_height) = Nu6.activation_height(network) else {
        return Amount::zero();
    };

    let expected_block_subsidy = block_subsidy_pre_zsf(nu6_activation_height, network)
        .expect("block at NU6 activation height must have valid expected subsidy");
    let &deferred_amount_per_block =
        funding_stream_values(nu6_activation_height, network, expected_block_subsidy)
            .expect("we always expect a funding stream hashmap response even if empty")
            .get(&FundingStreamReceiver::Deferred)
            .expect("we expect a lockbox funding stream after NU5");

    let post_nu6_funding_stream_height_range = network.post_nu6_funding_streams().height_range();

    // `min(height, last_height_with_deferred_pool_contribution) - (nu6_activation_height - 1)`,
    // We decrement NU6 activation height since it's an inclusive lower bound.
    // Funding stream height range end bound is not incremented since it's an exclusive end bound
    let num_blocks_with_lockbox_output = (height.0 + 1)
        .min(post_nu6_funding_stream_height_range.end.0)
        .checked_sub(post_nu6_funding_stream_height_range.start.0)
        .unwrap_or_default();

    (deferred_amount_per_block * num_blocks_with_lockbox_output.into())
        .expect("lockbox input value should fit in Amount")
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::Report;
    use zebra_chain::{
        amount::MAX_MONEY,
        parameters::testnet::{
            self, ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams,
        },
    };

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
            block_subsidy(
                (network.slow_start_interval() + 1).unwrap(),
                network,
                Amount::zero()
            )?
        );
        assert_eq!(
            Amount::<NonNegative>::try_from(1_250_000_000)?,
            block_subsidy((blossom_height - 1).unwrap(), network, Amount::zero())?
        );

        // After Blossom the block subsidy is reduced to 6.25 ZEC without halving
        // https://z.cash/upgrade/blossom/
        assert_eq!(
            Amount::<NonNegative>::try_from(625_000_000)?,
            block_subsidy(blossom_height, network, Amount::zero())?
        );

        // After the 1st halving, the block subsidy is reduced to 3.125 ZEC
        // https://z.cash/upgrade/canopy/
        assert_eq!(
            Amount::<NonNegative>::try_from(312_500_000)?,
            block_subsidy(first_halving_height, network, Amount::zero())?
        );

        // After the 2nd halving, the block subsidy is reduced to 1.5625 ZEC
        // See "7.8 Calculation of Block Subsidy and Founders' Reward"
        assert_eq!(
            Amount::<NonNegative>::try_from(156_250_000)?,
            block_subsidy(
                (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
                network,
                Amount::zero()
            )?
        );

        let zsf_activation_height = ZFuture
            .activation_height(network)
            .expect("ZFuture activation height should be available");

        // TODO: At ZSF activation the block subsidy is
        // assert_eq!(
        //     Amount::<NonNegative>::try_from()?,
        //     block_subsidy_pre_zsf(
        //         zsf_activation_height,
        //         network,
        //         zsf_balance_at_zsf_activation
        //     )?
        // );

        // After ZSF activation the block subsidy is 0 if ZSF balance is 0
        assert_eq!(
            Amount::<NonNegative>::try_from(0)?,
            block_subsidy(zsf_activation_height, network, Amount::zero())?
        );

        // After ZSF activation the block subsidy is 1 if ZSF balance is 1
        assert_eq!(
            Amount::<NonNegative>::try_from(1)?,
            block_subsidy(zsf_activation_height, network, 1i64.try_into().unwrap())?
        );

        // After ZSF activation the block subsidy is 866460000 zatoshis when ZSF balance is MAX_MONEY
        assert_eq!(
            Amount::<NonNegative>::try_from(866460000)?,
            block_subsidy(
                zsf_activation_height,
                network,
                MAX_MONEY.try_into().unwrap()
            )?
        );

        Ok(())
    }

    #[test]
    fn check_lockbox_input_value() -> Result<(), Report> {
        let _init_guard = zebra_test::init();

        let network = testnet::Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                blossom: Some(Blossom.activation_height(&Network::Mainnet).unwrap().0),
                nu6: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().start.0),
                ..Default::default()
            })
            .with_post_nu6_funding_streams(ConfiguredFundingStreams {
                // Start checking funding streams from block height 1
                height_range: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().clone()),
                // Use default post-NU6 recipients
                recipients: Some(
                    POST_NU6_FUNDING_STREAMS_TESTNET
                        .recipients()
                        .iter()
                        .map(|(&receiver, recipient)| ConfiguredFundingStreamRecipient {
                            receiver,
                            numerator: recipient.numerator(),
                            addresses: Some(
                                recipient
                                    .addresses()
                                    .iter()
                                    .map(|addr| addr.to_string())
                                    .collect(),
                            ),
                        })
                        .collect(),
                ),
            })
            .to_network();

        let nu6_height = Nu6.activation_height(&network).unwrap();
        let post_nu6_funding_streams = network.post_nu6_funding_streams();
        let height_range = post_nu6_funding_streams.height_range();

        let last_funding_stream_height = post_nu6_funding_streams
            .height_range()
            .end
            .previous()
            .expect("the previous height should be valid");

        assert_eq!(
            Amount::<NonNegative>::zero(),
            lockbox_input_value(&network, Height::MIN)
        );

        let expected_lockbox_value: Amount<NonNegative> = Amount::try_from(18_750_000)?;
        assert_eq!(
            expected_lockbox_value,
            lockbox_input_value(&network, nu6_height)
        );

        let num_blocks_total = height_range.end.0 - height_range.start.0;
        let expected_input_per_block: Amount<NonNegative> = Amount::try_from(18_750_000)?;
        let expected_lockbox_value = (expected_input_per_block * num_blocks_total.into())?;

        assert_eq!(
            expected_lockbox_value,
            lockbox_input_value(&network, last_funding_stream_height)
        );

        Ok(())
    }
}
