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

    assert_eq!(
        NetworkUpgrade::Nu6_3.target_spacing(),
        chrono::Duration::seconds(75)
    );
    assert_eq!(
        NetworkUpgrade::Nu7.target_spacing(),
        chrono::Duration::seconds(25)
    );

    assert_eq!(NetworkUpgrade::Nu6_3.averaging_window(), 17);
    assert_eq!(NetworkUpgrade::Nu7.averaging_window(), 102);

    // The wall-clock smoothing window is preserved across the transition: 17 * 75 == 1275 and
    // 102 * 25 == 2550. ZIP 218 deliberately doubles it, to halve the difficulty noise that the
    // 3x faster blocks would otherwise add.
    assert_eq!(
        NetworkUpgrade::Nu6_3.averaging_window_timespan(),
        chrono::Duration::seconds(1275)
    );
    assert_eq!(
        NetworkUpgrade::Nu7.averaging_window_timespan(),
        chrono::Duration::seconds(2550)
    );

    // The spacing change is visible through the height-dependent accessors too.
    let network = nu7_network(1_000);
    assert_eq!(
        NetworkUpgrade::target_spacing_for_height(&network, Height(999)),
        chrono::Duration::seconds(75)
    );
    assert_eq!(
        NetworkUpgrade::target_spacing_for_height(&network, Height(1_000)),
        chrono::Duration::seconds(25)
    );
    assert_eq!(
        NetworkUpgrade::averaging_window_for_height(&network, Height(999)),
        17
    );
    assert_eq!(
        NetworkUpgrade::averaging_window_for_height(&network, Height(1_000)),
        102
    );
}

/// ZIP 218 divides the block subsidy by a further factor of 3 from NU7, so that issuance per unit
/// of wall clock time is unchanged by the faster block spacing.
#[test]
fn zip_218_block_subsidy() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    let pre_nu7 = Height(999);
    let nu7 = Height(1_000);

    let pre_nu7_subsidy = block_subsidy(pre_nu7, &network)?;
    let nu7_subsidy = block_subsidy(nu7, &network)?;

    // NU7 activates well before the first halving on this network, so both heights are in the
    // same halving and the only difference is the ZIP 218 divisor.
    assert_eq!(halving(pre_nu7, &network), halving(nu7, &network));
    assert_eq!(nu7_subsidy, (pre_nu7_subsidy / 3)?);

    // Three post-NU7 blocks issue at most as much as one pre-NU7 block, so the issuance rate per
    // unit of wall clock time does not increase.
    assert!((nu7_subsidy * 3)? <= pre_nu7_subsidy);

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
    let pre_nu7 = Height(999);
    let nu7 = Height(1_000);

    // Before NU7, the miner keeps the whole of the fees.
    let fees = Amount::<NonNegative>::try_from(100_003)?;
    assert_eq!(
        nsm_fee_contribution(pre_nu7, &network, fees)?,
        Amount::<NonNegative>::zero(),
    );
    assert_eq!(miner_fees(pre_nu7, &network, fees)?, fees);

    // From NU7, 60% is removed, rounded down, so the miner keeps the rounding.
    assert_eq!(
        nsm_fee_contribution(nu7, &network, fees)?,
        Amount::<NonNegative>::try_from(60_001)?,
    );
    assert_eq!(
        miner_fees(nu7, &network, fees)?,
        Amount::<NonNegative>::try_from(40_002)?,
    );

    // The contribution and the miner's fees always add back up to the whole of the fees.
    for fees in [0, 1, 9, 10, 11, 999, 1_000_000_007] {
        let fees = Amount::<NonNegative>::try_from(fees)?;
        assert_eq!(
            (nsm_fee_contribution(nu7, &network, fees)? + miner_fees(nu7, &network, fees)?)?,
            fees,
            "the NSM contribution and the miner's fees must partition the fees",
        );
    }

    // A block with no fees contributes nothing.
    assert_eq!(
        nsm_fee_contribution(nu7, &network, Amount::<NonNegative>::zero())?,
        Amount::<NonNegative>::zero(),
    );

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
            nsm_subsidy(Height(2_000_000), network.nsm_reissuance_height(), reserve)?,
            Amount::<NonNegative>::zero(),
        );
    }

    let reissuance_height = Some(Height(1_000_000));

    // Nothing is reissued below the reissuance height.
    assert_eq!(
        nsm_subsidy(Height(999_999), reissuance_height, reserve)?,
        Amount::<NonNegative>::zero(),
    );

    // At and above it, the subsidy is `NSM_SUBSIDY_FRACTION` of the reserve: a reserve of
    // 10^10 zatoshi reissues exactly 1375 zatoshi per block.
    assert_eq!(
        nsm_subsidy(Height(1_000_000), reissuance_height, reserve)?,
        Amount::<NonNegative>::try_from(1375)?,
    );

    // Rounding is upward, so any positive reserve is eventually reissued, however small.
    assert_eq!(
        nsm_subsidy(
            Height(1_000_000),
            reissuance_height,
            Amount::<NonNegative>::try_from(1)?
        )?,
        Amount::<NonNegative>::try_from(1)?,
    );
    assert_eq!(
        nsm_subsidy(
            Height(1_000_000),
            reissuance_height,
            Amount::<NonNegative>::zero()
        )?,
        Amount::<NonNegative>::zero(),
    );

    Ok(())
}
