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
            height_for_halving, scheduled_block_subsidy, ParameterSubsidy as _,
        },
        testnet::ConfiguredActivationHeights,
        NetworkUpgrade,
    },
};

#[test]
fn funding_stream_p2pkh_recipient_is_matched_exactly() -> Result<(), Report> {
    use crate::{
        block::Block,
        parameters::{
            subsidy::{
                subsidy_is_valid, CoinbaseTransactionError, FundingStreamReceiver,
                FundingStreamRecipient, FundingStreams, SubsidyError,
            },
            testnet::Parameters,
            NetworkKind,
        },
        serialization::ZcashDeserializeInto,
        transaction::{LockTime, Transaction},
        transparent::{Address, Output},
    };
    use std::sync::Arc;

    let height = Height(1_046_400);
    let recipient = "t1MkHnkxVjNpNbCrSs3AJ8J7ZSp6NTYiUcG";
    // The private network needs a Testnet encoding of the same ZIP 2008 P2PKH script.
    let configured_recipient = Address::from_pub_key_hash(
        NetworkKind::Testnet,
        recipient.parse::<Address>()?.hash_bytes(),
    );
    let streams = FundingStreams::new(
        height..height.next()?,
        [(
            FundingStreamReceiver::MajorGrants,
            FundingStreamRecipient::new(8, [configured_recipient.to_string()]),
        )]
        .into_iter()
        .collect(),
    );
    let network = Parameters::build()
        .with_funding_streams(vec![(&streams).into()])
        .to_network()?;
    let subsidy = block_subsidy(height, &network)?;
    let payment = ((subsidy * 8)? / 100)?;
    let mut block: Block =
        zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES.zcash_deserialize_into()?;
    let inputs = block.transactions[0].inputs().to_vec();
    for (address, expected) in [
        (recipient, Ok(Default::default())),
        (
            "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow",
            Err(CoinbaseTransactionError::Subsidy(
                SubsidyError::FundingStreamNotFound,
            )),
        ),
    ] {
        let address: Address = address.parse()?;
        block.transactions[0] = Arc::new(Transaction::test_v4(
            inputs.clone(),
            vec![Output::new(payment, address.script())],
            LockTime::unlocked(),
            Height::MIN,
        ));
        assert_eq!(subsidy_is_valid(&block, &network, subsidy), expected);
    }
    Ok(())
}

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
        block_subsidy((network.slow_start_interval() + 1).unwrap(), network,)?
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
            network,
        )?
    );

    // After the 7th halving, the block subsidy is reduced to 0.04882812 ZEC
    // Check that the block subsidy rounds down correctly, and there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(4_882_812)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 6)).unwrap(),
            network,
        )?
    );

    // After the 29th halving, the block subsidy is 1 zatoshi
    // Check that the block subsidy is calculated correctly at the limit
    assert_eq!(
        Amount::<NonNegative>::try_from(1)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 28)).unwrap(),
            network,
        )?
    );

    // After the 30th halving, there is no block subsidy
    // Check that there are no errors
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 29)).unwrap(),
            network,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 39)).unwrap(),
            network,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 49)).unwrap(),
            network,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 59)).unwrap(),
            network,
        )?
    );

    // The largest possible integer divisor
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 62)).unwrap(),
            network,
        )?
    );

    // Other large divisors which should also result in zero
    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 63)).unwrap(),
            network,
        )?
    );

    assert_eq!(
        Amount::<NonNegative>::try_from(0)?,
        block_subsidy(
            (first_halving_height + (POST_BLOSSOM_HALVING_INTERVAL * 64)).unwrap(),
            network,
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
fn zip234_mainnet_like_testnet(nu7: u32) -> Network {
    zip234_mainnet_like_testnet_with_initial_balance(nu7, 35_080_000_000)
}

/// A Testnet like [`zip234_mainnet_like_testnet`], seeding the NSM value balance with
/// `initial_nsm_value_balance` zatoshis at its NU7 activation height.
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
fn zip234_pools_with_nsm_balance(nsm: i64) -> crate::value_balance::ValueBalance<NonNegative> {
    let mut pools = crate::value_balance::ValueBalance::<NonNegative>::zero();
    pools.set_nsm_amount(nsm.try_into().expect("valid amount"));
    pools
}

/// `BLOCK_SUBSIDY_FRACTION` is `LN2_SCALED / PostBlossomHalvingInterval`, which is 4126 / 10^10 on
/// Mainnet, and 1375 / 10^10 at ZIP 218's 25-second target spacing, where the halving interval
/// triples.

#[test]
fn check_zip234_block_subsidy_fraction_follows_halving_interval() -> Result<(), Report> {
    use crate::parameters::subsidy::{
        block_subsidy_fraction_numerator, ParameterSubsidy, LN2_SCALED,
    };

    let _init_guard = zebra_test::init();

    // Mainnet's post-Blossom halving interval is 1,680,000 blocks at 75-second spacing.
    assert_eq!(Network::Mainnet.post_blossom_halving_interval(), 1_680_000);
    assert_eq!(
        block_subsidy_fraction_numerator(Height(4_000_000), &Network::Mainnet),
        4126
    );

    // ZIP 218's 25-second target spacing triples the number of blocks in a halving period.
    let activation = Height(3_687_123);
    let zip218_like = zip234_mainnet_like_testnet(activation.0);
    assert_eq!(zip218_like.post_nu7_halving_interval(), 1_680_000 * 3);
    assert_eq!(
        block_subsidy_fraction_numerator(activation.previous()?, &zip218_like),
        4126
    );
    assert_eq!(
        block_subsidy_fraction_numerator(activation, &zip218_like),
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

#[test]
fn check_zip234_additional_block_subsidy_rounds_up() -> Result<(), Report> {
    use crate::{
        amount::MAX_MONEY,
        parameters::subsidy::{additional_block_subsidy, BLOCK_SUBSIDY_FRACTION_DENOMINATOR},
    };

    let _init_guard = zebra_test::init();

    // A Mainnet-like network with ZIP 218 active reissues at 1375 / 10^10.
    let height = Height(3_687_123);
    let network = zip234_mainnet_like_testnet(height.0);

    // `ceiling(1375 * balance / 10^10)`
    for (nsm_value_balance, additional) in [
        (0, 0),
        // Any non-zero balance reissues at least one zatoshi, so the balance always drains.
        (1, 1),
        (7_272_727, 1),
        (7_272_728, 2),
        (10_000_000_000_i64, 1375),
        (35_080_000_000_i64, 4824),
    ] {
        assert_eq!(
            additional_block_subsidy(height, &network, Amount::try_from(nsm_value_balance)?),
            Amount::<NonNegative>::try_from(additional)?,
            "NSM value balance: {nsm_value_balance}",
        );
    }

    // The largest interim value fits in 63 bits, as the ZIP's rationale requires.
    let interim = u128::try_from(MAX_MONEY)? * 1375;
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

#[test]
fn check_zip234_nsm_value_balance_change() -> Result<(), Report> {
    use std::ops::Neg;

    use crate::{
        amount::NegativeAllowed,
        parameters::subsidy::{
            additional_block_subsidy, nsm_fee_contribution, nsm_value_balance_change,
        },
        value_balance::ValueBalance,
    };

    let _init_guard = zebra_test::init();

    let activation = Height(3_687_123);
    let network = zip234_mainnet_like_testnet(activation.0);
    let initial = network.initial_nsm_value_balance();

    // Before activation, the balance doesn't change.
    assert_eq!(
        nsm_value_balance_change(
            (activation - 1).unwrap(),
            &network,
            ValueBalance::zero(),
            Amount::zero()
        )?,
        Amount::<NegativeAllowed>::zero(),
    );

    // The activation block seeds the balance and reissues from it in the same block.
    let seeded =
        nsm_value_balance_change(activation, &network, ValueBalance::zero(), Amount::zero())?;
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
        Amount::zero(),
    )?;

    assert_eq!(
        change,
        additional_block_subsidy((activation + 1).unwrap(), &network, nsm_value_balance)
            .constrain::<NegativeAllowed>()?
            .neg(),
    );

    // The fees ZIP 235 removes from circulation credit the balance.
    let fees = Amount::<NonNegative>::try_from(1_001)?;
    assert_eq!(
        nsm_value_balance_change(
            (activation + 1).unwrap(),
            &network,
            zip234_pools_with_nsm_balance(nsm_value_balance.into()),
            fees,
        )?,
        (change
            + nsm_fee_contribution((activation + 1).unwrap(), &network, fees)
                .constrain::<NegativeAllowed>()?)?,
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
        nsm_value_balance_change(nu7, &network, ValueBalance::zero(), Amount::zero())?,
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
            zip234_pools_with_nsm_balance(initial.into()),
            Amount::zero()
        )?,
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
            zip234_pools_with_nsm_balance(initial.into()),
            Amount::zero()
        )?,
        reissued.constrain::<NegativeAllowed>()?.neg(),
    );
    assert_eq!(
        block_subsidy(deployment, &network),
        Err(SubsidyError::ParentChainValuePoolsRequired(deployment)),
    );

    Ok(())
}

/// A deployment height before NU7 activation, or without one, is rejected.

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

#[test]
fn check_zip234_pool_halves_over_a_halving_period() -> Result<(), Report> {
    use crate::parameters::subsidy::{additional_block_subsidy, ParameterSubsidy};

    let _init_guard = zebra_test::init();

    // Simulating every block of a Mainnet halving period is slow, so use a network whose halving
    // interval is short enough to simulate, and check the same half-life property there.
    // Spacing upgrades activate at height 1, so disable slow start to keep the schedule valid.
    let short = crate::parameters::testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
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
    let short_interval = short.post_nu7_halving_interval();

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

    // Keep the early-upgrade fixture within the scheduled issuance cap.
    let configured = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
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

/// `NSMFeeContribution` is `floor(fees * 6 / 10)` of the block's total fees from NU7 activation,
/// so rounding favors the miner, and zero before it.

#[test]
fn check_zip235_nsm_fee_contribution() -> Result<(), Report> {
    use crate::{
        amount::MAX_MONEY,
        parameters::{
            subsidy::nsm_fee_contribution,
            testnet::{self, ConfiguredActivationHeights},
        },
    };

    let _init_guard = zebra_test::init();

    let nu7 = Height(3_687_123);
    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(nu7.0),
            ..(&Network::Mainnet.activation_list()).into()
        })?
        .to_network()?;

    for (fees, contribution) in [
        (0, 0_i64),
        (1, 0),
        (2, 1),
        (3, 1),
        (4, 2),
        (5, 3),
        (9, 5),
        (10, 6),
        (1_000, 600),
        (1_001, 600),
        (20_002, 12_001),
        (MAX_MONEY - 1, 1_259_999_999_999_999),
        (MAX_MONEY, 1_260_000_000_000_000),
    ] {
        let fees = Amount::<NonNegative>::try_from(fees)?;

        assert_eq!(
            nsm_fee_contribution(nu7, &network, fees),
            Amount::<NonNegative>::try_from(contribution)?,
        );
        assert!(nsm_fee_contribution((nu7 - 1).unwrap(), &network, fees).is_zero());
    }

    Ok(())
}

/// The NSM value balance change is an error, rather than a panic, when the seed and the fees
/// removed from circulation don't fit in an amount together.

#[test]
fn check_zip235_nsm_value_balance_change_overflow() -> Result<(), Report> {
    use crate::{
        amount::{NegativeAllowed, MAX_MONEY},
        parameters::subsidy::{
            additional_block_subsidy, nsm_fee_contribution, nsm_value_balance_change,
        },
        value_balance::ValueBalance,
    };

    let _init_guard = zebra_test::init();

    let activation = Height(3_687_123);
    let network = zip234_mainnet_like_testnet_with_initial_balance(activation.0, MAX_MONEY);
    let seed = Amount::<NonNegative>::try_from(MAX_MONEY)?;
    let reissued = additional_block_subsidy(activation, &network, seed);

    // The activation block reissues from the seed, which leaves room for a smaller contribution.
    let fees = Amount::<NonNegative>::try_from(2)?;
    assert_eq!(
        nsm_fee_contribution(activation, &network, fees),
        Amount::<NonNegative>::try_from(1)?
    );
    assert_eq!(
        nsm_value_balance_change(activation, &network, ValueBalance::zero(), fees)?,
        ((seed.constrain::<NegativeAllowed>()? - reissued.constrain::<NegativeAllowed>()?)?
            + Amount::<NegativeAllowed>::try_from(1)?)?,
    );

    // A contribution larger than the reissued amount takes the change over `MAX_MONEY`.
    let fees = (reissued + reissued)?;
    assert!(nsm_fee_contribution(activation, &network, fees) > reissued);
    assert!(nsm_value_balance_change(activation, &network, ValueBalance::zero(), fees).is_err());

    Ok(())
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

    let pre_nu7_subsidy = scheduled_block_subsidy(pre_nu7, &network)?;
    let nu7_subsidy = scheduled_block_subsidy(nu7, &network)?;

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

    let interval = network.funding_stream_address_change_interval() * 3;
    let first_halving = network.height_for_first_halving();
    let post_blossom = network.post_blossom_halving_interval();

    // The height whose numerator is exactly zero, and so the first height of period 0.
    let period_zero_start = (Height::MIN
        + (3 * (i64::from(first_halving.0) - post_blossom) - 2 * 50))
        .expect("the NU7-adjusted zero point is a valid height");

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
