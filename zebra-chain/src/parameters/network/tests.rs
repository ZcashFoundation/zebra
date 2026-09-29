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

/// A Testnet with Mainnet's activation heights, NU7 at `nu7`, and no funding streams or lockbox
/// disbursements, so its scheduled block subsidies match Mainnet's.
#[cfg(zcash_unstable = "zip234")]
fn zip234_mainnet_like_testnet(nu7: u32) -> Network {
    zip234_mainnet_like_testnet_with_initial_balance(nu7, 35_080_000_000)
}

/// A Testnet like [`zip234_mainnet_like_testnet`], seeding the NSM value balance with
/// `initial_nsm_value_balance` zatoshis at its NU7 activation height.
#[cfg(zcash_unstable = "zip234")]
fn zip234_mainnet_like_testnet_with_initial_balance(
    nu7: u32,
    initial_nsm_value_balance: i64,
) -> Network {
    use crate::parameters::testnet::{self, ConfiguredActivationHeights};

    testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(nu7),
            ..(&Network::Mainnet.activation_list()).into()
        })
        .unwrap()
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .with_initial_nsm_value_balance(initial_nsm_value_balance.try_into().expect("valid amount"))
        .to_network()
        .unwrap()
}

/// Chain value pools whose NSM value balance holds `nsm` zatoshis.
#[cfg(zcash_unstable = "zip234")]
fn zip234_pools_with_nsm_balance(nsm: i64) -> crate::value_balance::ValueBalance<NonNegative> {
    let mut pools = crate::value_balance::ValueBalance::<NonNegative>::zero();
    pools.set_nsm_amount(nsm.try_into().expect("valid amount"));
    pools
}

/// `BLOCK_SUBSIDY_FRACTION` is `LN2_SCALED / PostBlossomHalvingInterval`, which is 4126 / 10^10 on
/// Mainnet, and 1375 / 10^10 at ZIP 218's 25-second target spacing, where the halving interval
/// triples.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_block_subsidy_fraction_follows_halving_interval() -> Result<(), Report> {
    use crate::parameters::{
        subsidy::{block_subsidy_fraction_numerator, ParameterSubsidy, LN2_SCALED},
        testnet,
    };

    let _init_guard = zebra_test::init();

    // Mainnet's post-Blossom halving interval is 1,680,000 blocks at 75-second spacing.
    assert_eq!(Network::Mainnet.post_blossom_halving_interval(), 1_680_000);
    assert_eq!(
        block_subsidy_fraction_numerator(Height(4_000_000), &Network::Mainnet),
        4126
    );

    // ZIP 218's 25-second target spacing triples the number of blocks in a halving period.
    let zip218_like = testnet::Parameters::build()
        .with_halving_interval(840_000 * 3)?
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .to_network()?;
    assert_eq!(zip218_like.post_blossom_halving_interval(), 1_680_000 * 3);
    assert_eq!(
        block_subsidy_fraction_numerator(Height(1), &zip218_like),
        1375
    );

    // Regtest's short halving interval reissues the balance much faster.
    let regtest = Network::new_regtest(Default::default());
    assert_eq!(regtest.post_blossom_halving_interval(), 288);
    assert_eq!(
        block_subsidy_fraction_numerator(Height(1), &regtest),
        LN2_SCALED / 288,
    );

    // The numerator is non-zero for any halving interval up to `Height::MAX`.
    assert!(LN2_SCALED > u64::from(crate::block::Height::MAX_AS_U32));

    Ok(())
}

/// The additional block subsidy is `ceiling(BLOCK_SUBSIDY_FRACTION * NSMValueBalance)`.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_additional_block_subsidy_rounds_up() -> Result<(), Report> {
    use crate::{
        amount::MAX_MONEY,
        parameters::subsidy::{additional_block_subsidy, BLOCK_SUBSIDY_FRACTION_DENOMINATOR},
    };

    let _init_guard = zebra_test::init();

    // A Mainnet-like network that reissues from `height`, so the fraction is 4126 / 10^10.
    let height = Height(3_687_123);
    let network = zip234_mainnet_like_testnet(height.0);

    // `ceiling(4126 * balance / 10^10)`
    for (nsm_value_balance, additional) in [
        (0, 0),
        // Any non-zero balance reissues at least one zatoshi, so the balance always drains.
        (1, 1),
        (2_423_654, 1),
        (2_423_655, 2),
        (10_000_000_000_i64, 4126),
        (35_080_000_000_i64, 14_475),
    ] {
        assert_eq!(
            additional_block_subsidy(height, &network, Amount::try_from(nsm_value_balance)?),
            Amount::<NonNegative>::try_from(additional)?,
            "NSM value balance: {nsm_value_balance}",
        );
    }

    // The largest interim value fits in 63 bits, as the ZIP's rationale requires.
    let interim = u128::try_from(MAX_MONEY)? * 4126;
    assert!(interim < 1u128 << 63);
    assert_eq!(
        additional_block_subsidy(height, &network, Amount::try_from(MAX_MONEY)?),
        Amount::<NonNegative>::try_from(
            (interim as u64).div_ceil(BLOCK_SUBSIDY_FRACTION_DENOMINATOR)
        )?,
    );

    // The additional subsidy never exceeds the balance, so the balance cannot go negative.
    for nsm_value_balance in [1, 2, 4126, 10_000_000_000_i64, MAX_MONEY] {
        let nsm_value_balance = Amount::<NonNegative>::try_from(nsm_value_balance)?;
        assert!(
            additional_block_subsidy(Height(1), &network, nsm_value_balance) <= nsm_value_balance
        );
    }

    Ok(())
}

/// From activation, `BlockSubsidy(height)` is `ScheduledBlockSubsidy(height)` plus the additional
/// subsidy for `NSMValueBalance(height - 1)`, and the activation block reissues from
/// `INITIAL_NSM_VALUE_BALANCE`.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_block_subsidy_with_parent_pools() -> Result<(), Report> {
    use crate::{
        parameters::subsidy::{
            additional_block_subsidy, block_subsidy, block_subsidy_with_parent_pools,
            nsm_value_balance_before, nsm_value_balance_is_tracked, scheduled_block_subsidy,
            zip234_reissuance_is_active, SubsidyError,
        },
        value_balance::ValueBalance,
    };

    let _init_guard = zebra_test::init();

    let activation = Height(3_687_123);
    let network = zip234_mainnet_like_testnet(activation.0);
    let initial = network.initial_nsm_value_balance();

    // Mainnet and the default Testnet have no NU7 activation height and no deployment height, so
    // the balance is never tracked and the ZIP never reissues there.
    for network in [Network::Mainnet, Network::new_default_testnet()] {
        assert!(!nsm_value_balance_is_tracked(activation, &network));
        assert!(!zip234_reissuance_is_active(activation, &network));
        assert_eq!(network.zip234_deployment_height(), None);
    }

    // This network has no configured deployment height, so it reissues from NU7 activation.
    assert_eq!(network.zip234_deployment_height(), Some(activation));
    assert!(!nsm_value_balance_is_tracked(
        (activation - 1).unwrap(),
        &network
    ));
    assert!(!zip234_reissuance_is_active(
        (activation - 1).unwrap(),
        &network
    ));
    assert!(nsm_value_balance_is_tracked(activation, &network));
    assert!(zip234_reissuance_is_active(activation, &network));

    // Before activation the parent's pools are unused, and the subsidy is the scheduled one.
    let before_activation = (activation - 1).unwrap();
    assert_eq!(
        block_subsidy_with_parent_pools(
            before_activation,
            &network,
            zip234_pools_with_nsm_balance(12_345)
        )?,
        scheduled_block_subsidy(before_activation, &network)?,
    );

    // The activation block reissues from `INITIAL_NSM_VALUE_BALANCE`, not from the pools, which
    // do not hold the NSM balance yet.
    assert_eq!(
        nsm_value_balance_before(activation, &network, Amount::zero()),
        initial,
    );
    assert_eq!(
        block_subsidy_with_parent_pools(activation, &network, ValueBalance::zero())?,
        (scheduled_block_subsidy(activation, &network)?
            + additional_block_subsidy(activation, &network, initial))?,
    );

    // Later blocks reissue from the parent block's NSM value balance.
    let after_activation = (activation + 1).unwrap();
    let nsm_value_balance = Amount::<NonNegative>::try_from(10_000_000_000_i64)?;
    let mut parent_pools = ValueBalance::<NonNegative>::zero();
    parent_pools.set_nsm_amount(nsm_value_balance);

    assert_eq!(
        nsm_value_balance_before(after_activation, &network, parent_pools.nsm_amount()),
        nsm_value_balance,
    );
    assert_eq!(
        block_subsidy_with_parent_pools(after_activation, &network, parent_pools)?,
        (scheduled_block_subsidy(after_activation, &network)?
            + additional_block_subsidy(after_activation, &network, nsm_value_balance))?,
    );

    // An empty pool reissues nothing.
    assert_eq!(
        block_subsidy_with_parent_pools(after_activation, &network, ValueBalance::zero())?,
        scheduled_block_subsidy(after_activation, &network)?,
    );

    // `block_subsidy` can't answer once the subsidy depends on the parent's pools.
    assert_eq!(
        block_subsidy(activation, &network),
        Err(SubsidyError::ParentChainValuePoolsRequired(activation)),
    );

    Ok(())
}

/// The NSM value balance change debits the additional block subsidy, and credits
/// `INITIAL_NSM_VALUE_BALANCE` at the activation block, so that reissuance is a transfer between
/// chain value pools.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_nsm_value_balance_change() -> Result<(), Report> {
    use std::ops::Neg;

    use crate::{
        amount::NegativeAllowed,
        parameters::subsidy::{additional_block_subsidy, nsm_value_balance_change},
        value_balance::ValueBalance,
    };

    let _init_guard = zebra_test::init();

    let activation = Height(3_687_123);
    let network = zip234_mainnet_like_testnet(activation.0);
    let initial = network.initial_nsm_value_balance();

    // Before activation, the balance doesn't change.
    assert_eq!(
        nsm_value_balance_change((activation - 1).unwrap(), &network, ValueBalance::zero()),
        Amount::<NegativeAllowed>::zero(),
    );

    // The activation block seeds the balance and reissues from it in the same block.
    let seeded = nsm_value_balance_change(activation, &network, ValueBalance::zero());
    assert_eq!(
        seeded,
        (initial.constrain::<NegativeAllowed>()?
            - additional_block_subsidy(activation, &network, initial)
                .constrain::<NegativeAllowed>()?)?,
    );
    assert!(seeded > Amount::<NegativeAllowed>::zero());

    // Later blocks only debit the balance.
    let nsm_value_balance = Amount::<NonNegative>::try_from(10_000_000_000_i64)?;
    let change = nsm_value_balance_change(
        (activation + 1).unwrap(),
        &network,
        zip234_pools_with_nsm_balance(nsm_value_balance.into()),
    );

    assert_eq!(
        change,
        additional_block_subsidy((activation + 1).unwrap(), &network, nsm_value_balance)
            .constrain::<NegativeAllowed>()?
            .neg(),
    );

    // The pool balance after this block stays non-negative, as the ZIP requires.
    assert!(
        (nsm_value_balance.constrain::<NegativeAllowed>()? + change)?
            >= Amount::<NegativeAllowed>::zero()
    );

    Ok(())
}

/// With a deployment height after NU7 activation, the NSM value balance is seeded at NU7
/// activation but nothing is reissued until the deployment height, where reissuance starts from
/// the full balance.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_reissuance_starts_at_deployment_height() -> Result<(), Report> {
    use std::ops::Neg;

    use crate::{
        amount::NegativeAllowed,
        parameters::{
            subsidy::{
                additional_block_subsidy, block_subsidy, block_subsidy_with_parent_pools,
                nsm_value_balance_change, nsm_value_balance_is_tracked, scheduled_block_subsidy,
                zip234_reissuance_is_active, SubsidyError,
            },
            testnet::{self, ConfiguredActivationHeights},
        },
        value_balance::ValueBalance,
    };

    let _init_guard = zebra_test::init();

    let nu7 = Height(3_687_123);
    let deployment = Height(3_687_130);
    let initial = Amount::<NonNegative>::try_from(35_080_000_000_i64)?;

    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(nu7.0),
            ..(&Network::Mainnet.activation_list()).into()
        })?
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .with_initial_nsm_value_balance(initial)
        .with_zip234_deployment_height(deployment)
        .to_network()?;

    assert_eq!(network.zip234_deployment_height(), Some(deployment));

    // The NU7 activation block seeds the balance and reissues nothing.
    assert!(nsm_value_balance_is_tracked(nu7, &network));
    assert!(!zip234_reissuance_is_active(nu7, &network));
    assert!(additional_block_subsidy(nu7, &network, initial).is_zero());
    assert_eq!(
        nsm_value_balance_change(nu7, &network, ValueBalance::zero()),
        initial.constrain::<NegativeAllowed>()?,
    );
    assert_eq!(
        block_subsidy_with_parent_pools(nu7, &network, ValueBalance::zero())?,
        scheduled_block_subsidy(nu7, &network)?,
    );
    // The scheduled subsidy is still known without the parent's pools.
    assert_eq!(
        block_subsidy(nu7, &network)?,
        scheduled_block_subsidy(nu7, &network)?
    );

    // Between NU7 activation and deployment the balance is carried unchanged.
    let before_deployment = (deployment - 1).unwrap();
    assert_eq!(
        nsm_value_balance_change(
            before_deployment,
            &network,
            zip234_pools_with_nsm_balance(initial.into())
        ),
        Amount::<NegativeAllowed>::zero(),
    );

    // The deployment block reissues from the whole balance.
    assert!(zip234_reissuance_is_active(deployment, &network));
    let reissued = additional_block_subsidy(deployment, &network, initial);
    assert!(!reissued.is_zero());
    assert_eq!(
        block_subsidy_with_parent_pools(
            deployment,
            &network,
            zip234_pools_with_nsm_balance(initial.into())
        )?,
        (scheduled_block_subsidy(deployment, &network)? + reissued)?,
    );
    assert_eq!(
        nsm_value_balance_change(
            deployment,
            &network,
            zip234_pools_with_nsm_balance(initial.into())
        ),
        reissued.constrain::<NegativeAllowed>()?.neg(),
    );
    assert_eq!(
        block_subsidy(deployment, &network),
        Err(SubsidyError::ParentChainValuePoolsRequired(deployment)),
    );

    Ok(())
}

/// A deployment height before NU7 activation, or without one, is rejected.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_deployment_height_must_follow_nu7() -> Result<(), Report> {
    use crate::parameters::{
        network::error::ParametersBuilderError,
        testnet::{self, ConfiguredActivationHeights, RegtestParameters},
    };

    let _init_guard = zebra_test::init();

    let build = |nu7: Option<u32>, deployment: u32| {
        testnet::Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                nu7,
                ..(&Network::Mainnet.activation_list()).into()
            })
            .unwrap()
            .clear_funding_streams()
            .with_lockbox_disbursements(Vec::new())
            .with_zip234_deployment_height(Height(deployment))
            .to_network()
    };

    assert!(matches!(
        build(Some(3_687_123), 3_687_122),
        Err(ParametersBuilderError::Zip234DeploymentHeightBeforeNu7)
    ));
    assert!(matches!(
        build(None, 3_687_123),
        Err(ParametersBuilderError::Zip234DeploymentHeightBeforeNu7)
    ));
    assert!(build(Some(3_687_123), 3_687_123).is_ok());

    assert!(matches!(
        testnet::Parameters::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                nu7: Some(20),
                ..Default::default()
            },
            zip234_deployment_height: Some(Height(19)),
            ..Default::default()
        }),
        Err(ParametersBuilderError::Zip234DeploymentHeightBeforeNu7)
    ));

    Ok(())
}

/// The NSM value balance halves about once per halving period, and drains to zero without new removals.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_pool_halves_over_a_halving_period() -> Result<(), Report> {
    use crate::parameters::subsidy::{additional_block_subsidy, ParameterSubsidy};

    let _init_guard = zebra_test::init();

    // Simulating every block of a Mainnet halving period is slow, so use a network whose halving
    // interval is short enough to simulate, and check the same half-life property there.
    // It reissues from height 1.
    let short = crate::parameters::testnet::Parameters::build()
        .with_activation_heights(crate::parameters::testnet::ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(1),
            ..Default::default()
        })?
        .with_halving_interval(5_000)?
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .to_network()?;
    let short_interval = short.post_blossom_halving_interval();

    let start = Amount::<NonNegative>::try_from(35_080_000_000_i64)?;
    let mut balance = start;
    for _ in 0..short_interval {
        balance = (balance - additional_block_subsidy(Height(1), &short, balance))?;
    }

    // `(1 - ln 2 / n)^n` is within a fraction of a percent of one half for these intervals.
    let half = (start / 2)?;
    let tolerance = (start / 100)?;
    assert!(
        balance <= (half + tolerance)? && (balance + tolerance)? >= half,
        "after {short_interval} blocks the balance is {balance:?}, expected about {half:?}",
    );

    Ok(())
}

/// Mainnet and the default Testnet have no `DEPLOYMENT_BLOCK_HEIGHT` until the NU7 deployment ZIP
/// assigns one, so they never reissue, while a configured Testnet defaults to its NU7 activation
/// height.
#[cfg(zcash_unstable = "zip234")]
#[test]
fn check_zip234_deployment_height_is_unassigned_on_public_networks() -> Result<(), Report> {
    use crate::parameters::{
        subsidy::zip234_reissuance_is_active,
        testnet::{self, ConfiguredActivationHeights},
    };

    let _init_guard = zebra_test::init();

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        assert_eq!(network.zip234_deployment_height(), None);
        assert!(!zip234_reissuance_is_active(Height::MAX, &network));
    }

    let configured = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1_000),
            ..Default::default()
        })?
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .to_network()?;
    assert_eq!(configured.zip234_deployment_height(), Some(Height(1_000)));

    Ok(())
}
