//! Tests for funding streams.

#![allow(clippy::unwrap_in_result)]

use std::collections::HashMap;

use crate::amount::Amount;
use crate::parameters::NetworkUpgrade::*;
use crate::parameters::{subsidy::FundingStreamReceiver, NetworkKind};
use color_eyre::Report;

use super::*;

/// Checks that the Mainnet funding stream values are correct.
#[test]
fn test_funding_stream_values() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = &Network::Mainnet;

    let canopy_activation_height = Canopy.activation_height(network).unwrap();
    let nu6_activation_height = Nu6.activation_height(network).unwrap();
    let nu6_1_activation_height = Nu6_1.activation_height(network).unwrap();

    let dev_fund_height_range = network.all_funding_streams()[0].height_range();
    let nu6_fund_height_range = network.all_funding_streams()[1].height_range();
    let nu6_1_fund_height_range = network.all_funding_streams()[2].height_range();

    let nu6_fund_end = Height(3_146_400);
    let nu6_1_fund_end = Height(4_406_400);

    assert_eq!(canopy_activation_height, Height(1_046_400));
    assert_eq!(nu6_activation_height, Height(2_726_400));
    assert_eq!(nu6_1_activation_height, Height(3_146_400));

    assert_eq!(dev_fund_height_range.start, canopy_activation_height);
    assert_eq!(dev_fund_height_range.end, nu6_activation_height);

    assert_eq!(nu6_fund_height_range.start, nu6_activation_height);
    assert_eq!(nu6_fund_height_range.end, nu6_fund_end);

    assert_eq!(nu6_1_fund_height_range.start, nu6_1_activation_height);
    assert_eq!(nu6_1_fund_height_range.end, nu6_1_fund_end);

    assert_eq!(dev_fund_height_range.end, nu6_fund_height_range.start);

    let mut expected_dev_fund = HashMap::new();

    expected_dev_fund.insert(FundingStreamReceiver::Ecc, Amount::try_from(21_875_000)?);
    expected_dev_fund.insert(
        FundingStreamReceiver::ZcashFoundation,
        Amount::try_from(15_625_000)?,
    );
    expected_dev_fund.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(25_000_000)?,
    );
    let expected_dev_fund = expected_dev_fund;

    let mut expected_nu6_fund = HashMap::new();
    expected_nu6_fund.insert(
        FundingStreamReceiver::Deferred,
        Amount::try_from(18_750_000)?,
    );
    expected_nu6_fund.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(12_500_000)?,
    );
    let expected_nu6_fund = expected_nu6_fund;

    for height in [
        dev_fund_height_range.start.previous().unwrap(),
        dev_fund_height_range.start,
        dev_fund_height_range.start.next().unwrap(),
        dev_fund_height_range.end.previous().unwrap(),
        dev_fund_height_range.end,
        dev_fund_height_range.end.next().unwrap(),
        nu6_fund_height_range.start.previous().unwrap(),
        nu6_fund_height_range.start,
        nu6_fund_height_range.start.next().unwrap(),
        nu6_fund_height_range.end.previous().unwrap(),
        nu6_fund_height_range.end,
        nu6_fund_height_range.end.next().unwrap(),
        nu6_1_fund_height_range.start.previous().unwrap(),
        nu6_1_fund_height_range.start,
        nu6_1_fund_height_range.start.next().unwrap(),
        nu6_1_fund_height_range.end.previous().unwrap(),
        nu6_1_fund_height_range.end,
        nu6_1_fund_height_range.end.next().unwrap(),
    ] {
        let fsv = funding_stream_values(
            height,
            network,
            block_subsidy(height, network, Amount::zero())?,
        )
        .unwrap();

        if height < canopy_activation_height {
            assert!(fsv.is_empty());
        } else if height < nu6_activation_height {
            assert_eq!(fsv, expected_dev_fund);
        } else if height < nu6_1_fund_end {
            // NU6 and NU6.1 funding streams are in the same halving and expected to have the same values
            assert_eq!(fsv, expected_nu6_fund);
        } else {
            assert!(fsv.is_empty());
        }
    }

    Ok(())
}

/// Check mainnet and testnet funding stream addresses are valid transparent P2SH addresses.
#[test]
fn test_funding_stream_addresses() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        for (receiver, recipient) in network
            .all_funding_streams()
            .iter()
            .flat_map(|fs| fs.recipients())
        {
            for address in recipient.addresses() {
                let expected_network_kind = match network.kind() {
                    NetworkKind::Mainnet => NetworkKind::Mainnet,
                    // `Regtest` uses `Testnet` transparent addresses.
                    NetworkKind::Testnet | NetworkKind::Regtest => NetworkKind::Testnet,
                };

                assert_eq!(
                    address.network_kind(),
                    expected_network_kind,
                    "incorrect network for {receiver:?} funding stream address constant: {address}",
                );

                assert!(
                    address.is_script_hash(),
                    "funding stream address is not P2SH: {address}"
                );

                let _script = address.script();
            }
        }
    }

    Ok(())
}

//Test if funding streams ranges do not overlap
#[test]
fn test_funding_stream_ranges_dont_overlap() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        let funding_streams = network.all_funding_streams();
        // This is quadratic but it's fine since the number of funding streams is small.
        for i in 0..funding_streams.len() {
            for j in (i + 1)..funding_streams.len() {
                let range_a = funding_streams[i].height_range();
                let range_b = funding_streams[j].height_range();
                assert!(
                    // https://stackoverflow.com/a/325964
                    !(range_a.start < range_b.end && range_b.start < range_a.end),
                    "Funding streams {i} and {j} overlap: {range_a:?} and {range_b:?}",
                );
            }
        }
    }
    Ok(())
}

#[test]
fn cumulative_issuance_matches_the_schedule() -> Result<(), Report> {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

    assert_eq!(
        cumulative_scheduled_issuance(Height(2_726_399), &Network::Mainnet)?.zatoshis(),
        15_750_000 * 100_000_000i64,
    );
    let network = Parameters::build()
        .with_slow_start_interval(Height(20))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(130),
            nu7: Some(250),
            ..Default::default()
        })?
        .with_halving_interval(48)?
        .with_funding_streams(Vec::new())
        .to_network()?;
    let mut sum = Amount::zero();
    for height in 0..900 {
        sum = (sum + scheduled_block_subsidy(Height(height), &network)?)?;
        assert_eq!(
            cumulative_scheduled_issuance(Height(height), &network)?,
            sum
        );
    }
    Ok(())
}

#[test]
fn cumulative_issuance_stops_at_the_halving_limit_during_slow_start() -> Result<(), Report> {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

    let network = Parameters::build()
        .with_slow_start_interval(Height(200))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(200),
            nu7: Some(201),
            ..Default::default()
        })?
        .with_halving_interval(1)?
        .with_funding_streams(Vec::new())
        .to_network()?;
    let scheduled = (0..=200)
        .map(|height| scheduled_block_subsidy(Height(height), &network))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<Result<Amount<NonNegative>, _>>()?;
    assert_eq!(scheduled.zatoshis(), 83_937_500_000);
    assert_eq!(
        cumulative_scheduled_issuance(Height(200), &network)?,
        scheduled
    );
    Ok(())
}

#[test]
fn reissuance_height_requires_nu7_and_a_valid_height() {
    use crate::parameters::{
        network::error::ParametersBuilderError,
        testnet::{ConfiguredActivationHeights, Parameters},
    };

    let builder = Parameters::build().with_funding_streams(Vec::new());
    assert_eq!(
        builder
            .with_nsm_reissuance_height(Some(Height(1)))
            .to_network()
            .unwrap_err(),
        ParametersBuilderError::InvalidNsmReissuanceHeight,
    );
    for height in [Height(0), Height(99), Height(Height::MAX.0 + 1)] {
        let builder = Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                nu7: Some(100),
                ..Default::default()
            })
            .unwrap()
            .with_nsm_reissuance_height(Some(height))
            .with_funding_streams(Vec::new());
        assert_eq!(
            builder.to_network().unwrap_err(),
            ParametersBuilderError::InvalidNsmReissuanceHeight
        );
    }
}

#[test]
fn halving_intervals_reject_unserializable_and_unsupported_heights() {
    use crate::parameters::{network::error::ParametersBuilderError, testnet::Parameters};

    for interval in [
        0,
        HeightDiff::from(Height::MAX.0) + 1,
        HeightDiff::from(u32::MAX) + 1,
        HeightDiff::MAX / 6 + 1,
    ] {
        assert_eq!(
            Parameters::build()
                .with_halving_interval(interval)
                .unwrap_err(),
            ParametersBuilderError::InvalidHalvingInterval,
        );
    }
}

#[test]
fn funding_streams_require_a_nonzero_address_period() -> Result<(), Report> {
    use crate::parameters::{network::error::ParametersBuilderError, testnet::Parameters};

    let builder = Parameters::build().with_halving_interval(1)?;

    assert_eq!(
        builder.clone().to_network().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    assert_eq!(
        builder.clone().extend_funding_streams().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    let streamless = builder
        .with_funding_streams(Vec::new())
        .extend_funding_streams()?
        .to_network()?;
    let first_halving = streamless.height_for_first_halving();
    assert_eq!(halving(first_halving, &streamless), 1);
    assert_eq!(halving(first_halving.previous()?, &streamless), 0);
    Ok(())
}

#[test]
fn near_maximum_first_halvings_are_rejected() -> Result<(), Report> {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

    // Blossom at 1 and NU7 at 2 place the first halving at 6 * interval - 7.
    // Even the representable boundary overissues: representability alone is not sufficient.
    let maximum_interval = (HeightDiff::from(Height::MAX.0) + 7) / 6;
    let builder = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(2),
            ..Default::default()
        })?;
    for interval in [maximum_interval, maximum_interval + 1] {
        let invalid = builder
            .clone()
            .with_halving_interval(interval)?
            .with_funding_streams(Vec::new());
        assert!(invalid.clone().extend_funding_streams().is_err());
        assert!(invalid.to_network().is_err());
    }
    Ok(())
}

#[test]
fn scheduled_issuance_must_fit_the_monetary_cap() -> Result<(), Report> {
    use crate::parameters::{
        network::error::ParametersBuilderError,
        testnet::{ConfiguredActivationHeights, Parameters},
    };

    for (slow_start, interval) in [
        // The reviewed case has a representable first halving billions of blocks away.
        (0, HeightDiff::from(Height::MAX.0 / 2)),
        // Even a small interval increase can exceed the lifetime cap.
        (0, PRE_BLOSSOM_HALVING_INTERVAL + 1),
        // Slow start does not reduce subsidy when spacing upgrades activate early.
        (20_000, PRE_BLOSSOM_HALVING_INTERVAL),
    ] {
        let builder = Parameters::build()
            .with_slow_start_interval(Height(slow_start))
            .with_activation_heights(ConfiguredActivationHeights {
                nu6: Some(1),
                ..Default::default()
            })?
            .with_halving_interval(interval)?
            .with_funding_streams(Vec::new());
        assert_eq!(
            builder.clone().extend_funding_streams().unwrap_err(),
            ParametersBuilderError::InvalidSubsidySchedule,
        );
        assert_eq!(
            builder.to_network().unwrap_err(),
            ParametersBuilderError::InvalidSubsidySchedule,
        );
    }
    Ok(())
}

#[test]
fn reissuance_requires_a_positive_integer_coefficient() -> Result<(), Report> {
    use crate::parameters::{
        network::error::ParametersBuilderError,
        testnet::{ConfiguredActivationHeights, Parameters},
    };

    let interval = 1_155_280_001;
    let builder = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(interval),
            nu7: Some(interval + 1),
            ..Default::default()
        })?
        .with_halving_interval(HeightDiff::from(interval))?
        .with_nsm_reissuance_height(Some(Height(interval + 1)))
        .with_funding_streams(Vec::new());
    assert_eq!(
        builder.clone().extend_funding_streams().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    assert_eq!(
        builder.to_network().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    Ok(())
}

#[test]
fn monetary_validation_preserves_valid_schedules() -> Result<(), Report> {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters, RegtestParameters};

    let custom = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(1),
            ..Default::default()
        })?
        .with_funding_streams(Vec::new())
        .extend_funding_streams()?
        .to_network()?;
    let regtest = Network::new_configured_testnet(Parameters::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        },
        nsm_reissuance_height: Some(Height(1)),
        ..Default::default()
    })?);
    for network in [
        Network::Mainnet,
        Parameters::build().extend_funding_streams()?.to_network()?,
        Network::new_configured_testnet(Parameters::new_regtest(Default::default())?),
        custom,
        regtest.clone(),
    ] {
        let total = cumulative_scheduled_issuance_zatoshis(Height::MAX, &network)?;
        let genesis = u64::from(scheduled_block_subsidy(Height::MIN, &network)?);
        assert!(i128::from(total - genesis) <= i128::from(crate::amount::MAX_MONEY));
        assert_eq!(
            scheduled_block_subsidy(Height::MAX, &network)?,
            Amount::<NonNegative>::zero(),
        );
    }
    assert_eq!(
        nsm_subsidy(Height(1), &regtest, Amount::try_from(1)?)?,
        Amount::<NonNegative>::try_from(1)?,
    );
    Ok(())
}

#[test]
fn negative_halving_indices_are_rejected_before_issuance_validation() -> Result<(), Report> {
    use crate::parameters::{
        network::error::ParametersBuilderError,
        testnet::{ConfiguredActivationHeights, Parameters},
    };

    let builder = Parameters::build()
        .with_slow_start_interval(Height(20))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            canopy: Some(1),
            ..Default::default()
        })?
        .with_halving_interval(1)?
        .with_funding_streams(Vec::new());
    assert_eq!(
        builder.clone().extend_funding_streams().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    assert_eq!(
        builder.to_network().unwrap_err(),
        ParametersBuilderError::InvalidHalvingInterval,
    );
    Ok(())
}

#[test]
fn halving_heights_invert_slow_start_across_spacing_changes() -> Result<(), Report> {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

    for (blossom, nu7, first_halving) in [
        (100, None, 60),
        (60, None, 60),
        (50, None, 70),
        (50, Some(69), 72),
        (50, Some(70), 70),
        (50, Some(71), 70),
    ] {
        let network = Parameters::build()
            .with_slow_start_interval(Height(20))
            .with_activation_heights(ConfiguredActivationHeights {
                blossom: Some(blossom),
                canopy: Some(blossom),
                nu7,
                ..Default::default()
            })?
            .with_halving_interval(50)?
            .with_funding_streams(Vec::new())
            .to_network()?;
        assert_eq!(network.height_for_first_halving(), Height(first_halving));
        for index in 1..=4 {
            let height = height_for_halving(index, &network).unwrap();
            assert_eq!(halving(height, &network), index);
            assert_eq!(halving(height.previous()?, &network), index - 1);
        }
    }
    Ok(())
}

#[test]
fn zero_subsidy_still_requires_fixed_lockbox_disbursements() -> Result<(), Report> {
    use std::sync::Arc;

    use crate::{
        block::Header,
        parameters::testnet::{
            ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams, ConfiguredLockboxDisbursement, Parameters,
        },
        serialization::ZcashDeserializeInto,
        transaction::LockTime,
    };

    let height = Height(2_000);
    let address: Address = "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz".parse()?;
    let amount = Amount::<NonNegative>::try_from(100)?;
    let network = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            canopy: Some(1),
            nu6_1: Some(height.0),
            ..Default::default()
        })?
        .with_halving_interval(24)?
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(height..height.next()?),
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::ZcashFoundation,
                numerator: 5,
                addresses: Some(vec![address.to_string()]),
            }]),
        }])
        .with_lockbox_disbursements(vec![ConfiguredLockboxDisbursement {
            address: address.to_string(),
            amount,
        }])
        .to_network()?;
    let subsidy = scheduled_block_subsidy(height, &network)?;
    assert_eq!(subsidy, Amount::<NonNegative>::zero());
    let header: Header = zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
    let make_block = |outputs| Block {
        header: Arc::new(header),
        transactions: vec![Arc::new(Transaction::test_v1(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            outputs,
            LockTime::unlocked(),
        ))],
    };
    assert_eq!(
        subsidy_is_valid(&make_block(Vec::new()), &network, subsidy),
        Err(SubsidyError::OneTimeLockboxDisbursementNotFound),
    );
    // No zero-valued proportional funding output is required alongside the fixed payout.
    let paid = make_block(vec![Output::new(amount, address.script())]);
    let deferred = DeferredPoolBalanceChange::new(Amount::try_from(-100)?);
    assert_eq!(subsidy_is_valid(&paid, &network, subsidy)?, deferred);
    miner_fees_are_valid(
        &paid.transactions[0],
        height,
        Amount::zero(),
        subsidy,
        deferred,
        &network,
    )?;
    Ok(())
}
