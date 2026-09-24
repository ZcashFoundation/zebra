#![allow(clippy::unwrap_in_result)]

mod prop;
mod vectors;

use color_eyre::Report;

use super::Network;
use crate::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{
        subsidy::{
            block_subsidy, constants::POST_BLOSSOM_HALVING_INTERVAL, halving, halving_divisor,
            height_for_halving, ParameterSubsidy as _,
        },
        testnet::ConfiguredActivationHeights,
        NetworkUpgrade,
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
    let blossom_height = NetworkUpgrade::Blossom.activation_height(network).unwrap();
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
    let blossom_height = NetworkUpgrade::Blossom.activation_height(network).unwrap();
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
            Amount::zero(),
        )?
    );

    // After the 7th halving, the block subsidy is reduced to 0.04882812 ZEC
    // Check that the block subsidy rounds down correctly, and there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(4_882_812)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 6)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    // After the 29th halving, the block subsidy is 1 zatoshi
    // Check that the block subsidy is calculated correctly at the limit
    assert_eq!(
        Amount::<NonNegative>::try_from(1)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 28)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    // After the 30th halving, there is no block subsidy
    // Check that there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 29)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 39)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 49)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 59)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    // The largest possible integer divisor
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 62)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    // Other large divisors which should also result in zero
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 63)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 64)).unwrap(),
            network,
            Amount::zero(),
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height(Height::MAX_AS_U32 / 4), network, Amount::zero())?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height(Height::MAX_AS_U32 / 2), network, Amount::zero())?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height::MAX, network, Amount::zero())?
    );

    Ok(())
}

#[test]
fn check_height_for_num_halvings() {
    for network in Network::iter() {
        for h in 1..1000 {
            let Some(height_for_halving) = height_for_halving(h, &network) else {
                panic!("could not find height for halving {h}");
            };

            let prev_height = height_for_halving
                .previous()
                .expect("there should be a previous height");

            assert_eq!(
                h,
                halving(height_for_halving, &network),
                "num_halvings should match the halving index"
            );

            assert_eq!(
                h - 1,
                halving(prev_height, &network),
                "num_halvings for the prev height should be 1 less than the halving index"
            );
        }
    }
}

/// Builds a Regtest network with NU7 activating at `nu7_height`, so [ZIP 218]'s post-NU7 era can
/// be exercised while NU7 is unscheduled on Mainnet and the default Testnet.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
fn nu7_network(nu7_height: u32) -> Network {
    Network::new_regtest(
        crate::parameters::testnet::ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(nu7_height),
            ..Default::default()
        }
        .into(),
    )
}

/// ZIP 218 drops the target block spacing to 25 seconds and widens the difficulty averaging
/// window to 102 blocks from NU7 onward.
#[test]
fn zip_218_target_spacing_and_averaging_window() {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    // ZIP 218 triples the block rate and doubles the wall-clock smoothing window.
    for (upgrade, height, spacing, window, timespan) in [
        (NetworkUpgrade::Nu6_3, Height(999), 75, 17, 1275),
        (NetworkUpgrade::Nu7, Height(1_000), 25, 102, 2550),
    ] {
        let spacing = chrono::Duration::seconds(spacing);
        assert_eq!(upgrade.target_spacing(), spacing);
        assert_eq!(upgrade.averaging_window(), window);
        assert_eq!(
            upgrade.averaging_window_timespan(),
            chrono::Duration::seconds(timespan)
        );
        assert_eq!(
            NetworkUpgrade::target_spacing_for_height(&network, height),
            spacing
        );
        assert_eq!(
            NetworkUpgrade::averaging_window_for_height(&network, height),
            window
        );
    }
}

/// ZIP 218 divides the block subsidy by a further factor of 3 from NU7, so that issuance per unit
/// of wall clock time is unchanged by the faster block spacing.
#[test]
fn zip_218_block_subsidy() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    let pre_nu7 = Height(999);
    let nu7 = Height(1_000);

    let pre_nu7_subsidy = block_subsidy(pre_nu7, &network, Amount::zero())?;
    let nu7_subsidy = block_subsidy(nu7, &network, Amount::zero())?;

    // Both heights are in the same halving, so only the ZIP 218 divisor changes.
    assert_eq!(halving(pre_nu7, &network), halving(nu7, &network));
    assert_eq!(nu7_subsidy, (pre_nu7_subsidy / 3)?);

    Ok(())
}

/// ZIP 218 triples the halving interval at NU7, so the wall-clock time between halvings is
/// unchanged, and `halving()` and `height_for_halving()` stay consistent across the boundary.
#[test]
fn zip_218_halving_interval() {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);

    assert_eq!(
        network.post_nu7_halving_interval(),
        network.post_blossom_halving_interval() * 3,
    );

    // The halving index does not jump at the activation height.
    assert_eq!(
        halving(Height(999), &network),
        halving(Height(1_000), &network),
    );

    for h in 1..20 {
        let height = height_for_halving(h, &network).expect("halving height is representable");
        let previous = height.previous().expect("there is a previous height");

        assert_eq!(
            halving(height, &network),
            h,
            "the halving index at height_for_halving({h}) should be {h}",
        );
        assert_eq!(
            halving(previous, &network),
            h - 1,
            "the halving index just below height_for_halving({h}) should be {}",
            h - 1,
        );
    }
}

/// The NU7 deployment ZIP removes 60% of a block's transaction fees from circulation into the NSM
/// reserve, rounding in the miner's favour, and leaves the fees untouched before NU7.
#[test]
fn nsm_fee_contribution_and_miner_fees() -> Result<(), Report> {
    use crate::parameters::subsidy::{miner_fees, nsm_fee_contribution};

    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    // Before NU7 all fees go to the miner; afterward 60% goes to the reserve, rounded down.
    for (height, fees, contribution) in [
        (999, 100_003, 0),
        (1_000, 100_003, 60_001),
        (1_000, 0, 0),
        (1_000, 1, 0),
        (1_000, 9, 5),
        (1_000, 10, 6),
        (1_000, 11, 6),
        (1_000, 999, 599),
        (1_000, 1_000_000_007, 600_000_004),
    ] {
        let fees = Amount::<NonNegative>::try_from(fees)?;
        let contribution = Amount::<NonNegative>::try_from(contribution)?;
        assert_eq!(
            nsm_fee_contribution(Height(height), &network, fees)?,
            contribution
        );
        assert_eq!(
            miner_fees(Height(height), &network, fees)?,
            (fees - contribution)?
        );
    }

    Ok(())
}

/// The NSM reissuance height is unassigned, so no reserve is reissued yet; when it is assigned,
/// the per-block subsidy is the reserve balance times the NSM fraction, rounded up.
#[test]
fn nsm_subsidy_reissuance() -> Result<(), Report> {
    use crate::parameters::subsidy::nsm_subsidy;

    let _init_guard = zebra_test::init();

    let reserve = Amount::<NonNegative>::try_from(10_000_000_000_i64)?;

    // No network has an assigned reissuance height, so nothing is reissued anywhere.
    for network in Network::iter() {
        assert_eq!(network.nsm_reissuance_height(), None);
        assert_eq!(
            nsm_subsidy(Height(2_000_000), &network, reserve)?,
            Amount::<NonNegative>::zero(),
        );
    }

    let network = crate::parameters::testnet::Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1_000_000),
            ..Default::default()
        })?
        .with_nsm_reissuance_height(Some(Height(1_000_000)))
        .with_funding_streams(Vec::new())
        .to_network()?;

    // Reissuance starts at the configured height and rounds up, even for a one-zatoshi reserve.
    for (height, reserve, expected) in [
        (999_999, 10_000_000_000_i64, 0),
        (1_000_000, 10_000_000_000, 1375),
        (1_000_000, 1, 1),
        (1_000_000, 0, 0),
    ] {
        assert_eq!(
            nsm_subsidy(Height(height), &network, Amount::try_from(reserve)?)?,
            Amount::<NonNegative>::try_from(expected)?,
        );
    }

    Ok(())
}

/// The funding stream address period is a `floor`, not a truncation: the two spec periods either
/// side of zero must not be merged, because both callers use the period only as a difference.
///
/// The period is never negative on Mainnet, the default Testnet or Regtest. It can be on a
/// configured Testnet where ZIP 218 stretches the first halving above the post-Blossom halving
/// interval, which is the case this pins.
#[test]
fn funding_stream_address_period_floors_negative_heights() {
    use crate::parameters::{
        subsidy::funding_stream_address_period,
        testnet::{ConfiguredActivationHeights, Parameters},
    };

    let _init_guard = zebra_test::init();

    // A Testnet with NU7 well before the first halving, so ZIP 218 triples the remaining blocks
    // of that halving and pushes `height_for_first_halving()` far above the post-Blossom halving
    // interval. Funding streams are left empty, so no address period is ever used in anger here.
    let network = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(30),
            nu5: Some(35),
            nu6: Some(40),
            nu6_1: Some(45),
            nu6_2: Some(47),
            nu6_3: Some(48),
            nu7: Some(50),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("failed to build configured network");

    let interval = network.funding_stream_address_change_interval();
    let first_halving = network.height_for_first_halving();
    let post_blossom = network.post_blossom_halving_interval();

    // The height whose numerator is exactly zero, and so the first height of period 0.
    let period_zero_start =
        (first_halving - post_blossom).expect("the zero point is a valid height");

    // Include both ends of periods -1 and 0: truncation would merge them at zero.
    for (offset, expected) in [
        (0, 0),
        (interval - 1, 0),
        (interval, 1),
        (-1, -1),
        (-interval, -1),
        (-interval - 1, -2),
    ] {
        let height = (period_zero_start + offset).expect("valid height");
        assert_eq!(
            funding_stream_address_period(height, &network),
            expected,
            "incorrect funding stream period at {height:?}",
        );
    }
}

/// `height_for_first_halving()` derives the height from `height_for_halving(1)` rather than
/// hard-coding it. These are the heights the spec gives, so deriving them must not move any of
/// them.
#[test]
fn first_halving_heights_are_unchanged() {
    let _init_guard = zebra_test::init();

    // Mainnet's first halving is at Canopy; the Testnet height is from protocol specification
    // §7.10.1 <https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams>.
    for (network, expected) in [
        (Network::Mainnet, Height(1_046_400)),
        (Network::new_default_testnet(), Height(1_116_000)),
        (Network::new_regtest(Default::default()), Height(287)),
    ] {
        assert_eq!(
            network.height_for_first_halving(),
            expected,
            "the first halving height must not move on {network}",
        );
    }

    assert_eq!(
        Network::Mainnet.height_for_first_halving(),
        NetworkUpgrade::Canopy
            .activation_height(&Network::Mainnet)
            .expect("Canopy is activated on Mainnet"),
    );
}

/// `height_for_first_halving()` must agree with `halving()` even when ZIP 218 stretches the
/// schedule, which a hard-coded height could not: NU7 activating before the first halving pushes
/// it later, and both have to follow.
#[test]
fn first_halving_follows_the_zip_218_schedule() {
    let _init_guard = zebra_test::init();

    // NU7 activates at height 1, so every block of the first halving is stretched by ZIP 218.
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        }
        .into(),
    );

    let first_halving = network.height_for_first_halving();
    let unstretched = Network::new_regtest(Default::default()).height_for_first_halving();

    assert!(
        first_halving > unstretched,
        "ZIP 218 should push the first halving {first_halving:?} past its unstretched height \
         {unstretched:?}",
    );

    // The height it reports is the one `halving()` actually treats as the first halving.
    assert_eq!(halving(first_halving, &network), 1);
    assert_eq!(
        halving(
            first_halving
                .previous()
                .expect("the first halving is above genesis"),
            &network
        ),
        0,
    );
}
