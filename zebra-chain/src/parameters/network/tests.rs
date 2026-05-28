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
        block_subsidy((network.slow_start_interval() + 1).unwrap(), network, None)?
    );
    assert_eq!(
        Amount::<NonNegative>::try_from(1_250_000_000)?,
        block_subsidy((blossom_height - 1).unwrap(), network, None)?
    );

    // After Blossom the block subsidy is reduced to 6.25 ZEC without halving
    // https://z.cash/upgrade/blossom/
    assert_eq!(
        Amount::<NonNegative>::try_from(625_000_000)?,
        block_subsidy(blossom_height, network, None)?
    );

    // After the 1st halving, the block subsidy is reduced to 3.125 ZEC
    // https://z.cash/upgrade/canopy/
    assert_eq!(
        Amount::<NonNegative>::try_from(312_500_000)?,
        block_subsidy(first_halving_height, network, None)?
    );

    // After the 2nd halving, the block subsidy is reduced to 1.5625 ZEC
    // See "7.8 Calculation of Block Subsidy and Founders' Reward"
    assert_eq!(
        Amount::<NonNegative>::try_from(156_250_000)?,
        block_subsidy(
            (first_halving_height + POST_BLOSSOM_HALVING_INTERVAL).unwrap(),
            network,
            None,
        )?
    );

    // After the 7th halving, the block subsidy is reduced to 0.04882812 ZEC
    // Check that the block subsidy rounds down correctly, and there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(4_882_812)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 6)).unwrap(),
            network,
            None,
        )?
    );

    // After the 29th halving, the block subsidy is 1 zatoshi
    // Check that the block subsidy is calculated correctly at the limit
    assert_eq!(
        Amount::<NonNegative>::try_from(1)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 28)).unwrap(),
            network,
            None,
        )?
    );

    // After the 30th halving, there is no block subsidy
    // Check that there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 29)).unwrap(),
            network,
            None,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 39)).unwrap(),
            network,
            None,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 49)).unwrap(),
            network,
            None,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 59)).unwrap(),
            network,
            None,
        )?
    );

    // The largest possible integer divisor
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 62)).unwrap(),
            network,
            None
        )?
    );

    // Other large divisors which should also result in zero
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 63)).unwrap(),
            network,
            None
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 64)).unwrap(),
            network,
            None
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height(Height::MAX_AS_U32 / 4), network, None)?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height(Height::MAX_AS_U32 / 2), network, None)?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(Height::MAX, network, None)?
    );

    Ok(())
}

#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_block_subsidy_reserve_decreases() -> Result<(), Report> {
    use crate::{amount::MAX_MONEY, parameters::subsidy::zip234_start_height};

    let network = zip234_testnet(1);
    let mut reserve: Amount<NonNegative> = MAX_MONEY.try_into()?;
    let first_height = zip234_start_height(&network).expect("NU7 configured");
    let last_height = Height::MAX;

    let mut heights = vec![first_height, last_height];
    let mut h = first_height;
    while h < last_height {
        heights.push(h);
        h = (h + 100_000).unwrap_or(last_height)
    }

    heights.sort_unstable();
    heights.dedup();

    let mut prev_subsidy = reserve;
    for &height in &heights {
        let subsidy = block_subsidy(height, &network, Some(reserve))?;
        assert!(
            subsidy < reserve,
            "subsidy ({subsidy:?}) exceeds reserve ({reserve:?}) at height ({height:?})"
        );

        assert!(
            subsidy < prev_subsidy,
            "subsidy ({subsidy:?}) did not decrease at height ({height:?})"
        );

        prev_subsidy = subsidy;

        let result = reserve.checked_sub(subsidy).expect("reserve went negative");
        reserve = Amount::<NonNegative>::try_from(i64::from(result))
            .expect("reserve should be non-negative");
    }

    assert!(
        reserve >= Amount::<NonNegative>::zero(),
        "reserve should be non-negative"
    );

    Ok(())
}

/// Testnet with NU7 at `nu7` and no funding streams.
#[cfg(zcash_unstable = "zip234")]
fn zip234_testnet(nu7: u32) -> Network {
    use crate::parameters::testnet::{self, ConfiguredActivationHeights};
    testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(nu7),
            ..Default::default()
        })
        .unwrap()
        .clear_funding_streams()
        .to_network()
        .unwrap()
}

/// ZIP-234 start = `height_for_halving(halving(nu7) + 2, network)`.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_start_height_derivation() -> Result<(), Report> {
    use crate::parameters::subsidy::{halving, zip234_start_height};

    for nu7 in [1u32, 100, 500_000, 1_000_000] {
        let net = zip234_testnet(nu7);
        let nu7_height = Height(nu7);
        let expected = height_for_halving(halving(nu7_height, &net) + 2, &net)
            .expect("derivation should not overflow for in-range NU7");
        assert_eq!(
            zip234_start_height(&net),
            Some(expected),
            "derivation mismatch for NU7={nu7}",
        );
    }

    Ok(())
}

/// At `start − 1`: halving path, reserve ignored. At `start`: smoothed formula.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_block_subsidy_zip234_activation_boundary() -> Result<(), Report> {
    use crate::parameters::subsidy::zip234_start_height;

    let net = zip234_testnet(1);
    let start = zip234_start_height(&net).expect("NU7 configured");
    let pre_activation = (start - 1).unwrap();

    let pre = block_subsidy(pre_activation, &net, None)?;
    let pre_with_reserve =
        block_subsidy(pre_activation, &net, Some(Amount::try_from(123_456)?))?;
    assert_eq!(pre, pre_with_reserve);

    let reserve = Amount::<NonNegative>::try_from(10_000_000_000_i64)?;
    let subsidy = block_subsidy(start, &net, Some(reserve))?;
    assert_eq!(subsidy, Amount::<NonNegative>::try_from(4_126)?);

    Ok(())
}

/// Ceiling rounding: reserve = 1 → subsidy = 1 (floor would give 0).
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_block_subsidy_zip234_ceiling_at_one_zat() -> Result<(), Report> {
    use crate::parameters::subsidy::zip234_start_height;

    let net = zip234_testnet(1);
    let height = zip234_start_height(&net).expect("NU7 configured");
    let one = Amount::<NonNegative>::try_from(1)?;
    assert_eq!(block_subsidy(height, &net, Some(one))?, one);
    Ok(())
}

/// Reserve = 0 → subsidy = 0.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_block_subsidy_zip234_zero_reserve() -> Result<(), Report> {
    use crate::parameters::subsidy::zip234_start_height;

    let net = zip234_testnet(1);
    let height = zip234_start_height(&net).expect("NU7 configured");
    let zero = Amount::<NonNegative>::zero();
    assert_eq!(block_subsidy(height, &net, Some(zero))?, zero);
    Ok(())
}

/// Reserve = 10¹⁰ → subsidy = 4126 (pins the exact spec rational).
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_block_subsidy_zip234_exact_fraction() -> Result<(), Report> {
    use crate::parameters::subsidy::zip234_start_height;

    let net = zip234_testnet(1);
    let height = zip234_start_height(&net).expect("NU7 configured");
    let reserve = Amount::<NonNegative>::try_from(10_000_000_000_i64)?;
    let expected = Amount::<NonNegative>::try_from(4_126)?;
    assert_eq!(block_subsidy(height, &net, Some(reserve))?, expected);
    Ok(())
}

/// ZIP-235 burns increase money reserve by exactly the burned amount.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_money_reserve_recovers_from_burns() -> Result<(), Report> {
    use crate::{
        amount::NegativeAllowed, parameters::subsidy::zip234_start_height,
        value_balance::ValueBalance,
    };

    let net = zip234_testnet(1);
    let height = zip234_start_height(&net).expect("NU7 configured");

    let initial_pool = ValueBalance::<NonNegative>::zero();
    let initial_reserve = initial_pool.money_reserve();

    let subsidy = block_subsidy(height, &net, Some(initial_reserve))?;
    let subsidy_signed: Amount<NegativeAllowed> = subsidy.constrain()?;

    let pool_without_burn = initial_pool.add_chain_value_pool_change(
        ValueBalance::<NegativeAllowed>::from_transparent_amount(subsidy_signed),
    )?;
    let reserve_without_burn = pool_without_burn.money_reserve();

    let burned = Amount::<NonNegative>::try_from(1_000_000_i64)?;
    let burned_signed: Amount<NegativeAllowed> = burned.constrain()?;
    let pool_with_burn = initial_pool.add_chain_value_pool_change(
        ValueBalance::<NegativeAllowed>::from_transparent_amount(
            (subsidy_signed - burned_signed)?,
        ),
    )?;
    let reserve_with_burn = pool_with_burn.money_reserve();

    assert!(reserve_with_burn > reserve_without_burn);
    let recovery: Amount<NegativeAllowed> = (reserve_with_burn.constrain::<NegativeAllowed>()?
        - reserve_without_burn.constrain::<NegativeAllowed>()?)?;
    assert_eq!(recovery, burned_signed);

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
