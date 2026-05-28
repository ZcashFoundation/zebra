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
        testnet::{self, ConfiguredActivationHeights},
        NetworkUpgrade, NU7_POW_TARGET_SPACING_RATIO, POST_BLOSSOM_POW_TARGET_SPACING,
        POST_NU7_POW_TARGET_SPACING,
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

/// Tests `halving` and `block_subsidy` across the NU7 activation boundary on a
/// configured Testnet, exercising the post-NU7 cases added by the draft
/// "Shorter Block Target Spacing" ZIP.
#[test]
fn post_nu7_halving_and_subsidy() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Pick parameters where slow_start_shift = blossom_height, so the pre-Blossom
    // term in the spec halving sum is exactly zero and boundaries land cleanly at
    // multiples of the post-Blossom / post-NU7 halving intervals.
    let blossom = 1u32;
    let canopy = blossom + u32::try_from(POST_BLOSSOM_HALVING_INTERVAL).unwrap();
    let nu7 = canopy + u32::try_from(POST_BLOSSOM_HALVING_INTERVAL * 2).unwrap();

    let network = testnet::Parameters::build()
        // slow_start_shift = slow_start_interval / 2 = 1, equal to Blossom height.
        .with_slow_start_interval(Height(2))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(blossom),
            canopy: Some(canopy),
            nu7: Some(nu7),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .clear_funding_streams()
        .to_network()
        .expect("configured testnet is valid");

    let nu7_height = Height(nu7);

    // Target spacing shortens from the post-Blossom value to the post-NU7 value
    // exactly at the NU7 activation height.
    assert_eq!(
        i64::from(POST_BLOSSOM_POW_TARGET_SPACING),
        NetworkUpgrade::target_spacing_for_height(&network, (nu7_height - 1).unwrap())
            .num_seconds()
    );
    assert_eq!(
        i64::from(POST_NU7_POW_TARGET_SPACING),
        NetworkUpgrade::target_spacing_for_height(&network, nu7_height).num_seconds()
    );

    // At NU7 activation, three post-Blossom halvings have elapsed:
    //   Halving = floor(0/PreBlossom + 1 + 2) = 3
    assert_eq!(3, halving(nu7_height, &network));
    assert_eq!(8, halving_divisor(nu7_height, &network).unwrap());

    // BlockSubsidy(NU7) = floor(MAX / (BlossomRatio * NU7Ratio * 2^Halving))
    //                   = floor(1_250_000_000 / (2 * 3 * 8))
    //                   = floor(1_250_000_000 / 48) = 26_041_666 zatoshi
    assert_eq!(
        Amount::<NonNegative>::try_from(26_041_666)?,
        block_subsidy(nu7_height, &network)?,
    );

    // The 3rd halving boundary lands exactly at NU7 in this configuration, so the
    // block immediately before NU7 is still in halving era 2 (post-Blossom, pre-NU7):
    //   floor(1_250_000_000 / (2 * 4)) = 156_250_000 zatoshi.
    assert_eq!(2, halving((nu7_height - 1).unwrap(), &network));
    assert_eq!(
        Amount::<NonNegative>::try_from(156_250_000)?,
        block_subsidy((nu7_height - 1).unwrap(), &network)?,
    );

    // The halving counter does not reset at NU7. The next halving boundary is
    // reached after one PostNU7HalvingInterval (=PostBlossomHalvingInterval * 3)
    // of post-NU7 blocks.
    let post_nu7_halving_interval =
        POST_BLOSSOM_HALVING_INTERVAL * i64::from(NU7_POW_TARGET_SPACING_RATIO);
    let next_halving = (nu7_height + post_nu7_halving_interval).unwrap();
    assert_eq!(4, halving(next_halving, &network));
    assert_eq!(16, halving_divisor(next_halving, &network).unwrap());
    assert_eq!(
        Amount::<NonNegative>::try_from(13_020_833)?,
        block_subsidy(next_halving, &network)?,
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
