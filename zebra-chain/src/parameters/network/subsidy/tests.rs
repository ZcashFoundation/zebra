//! Tests for scheduled subsidy arithmetic and parameter validation.

#![allow(clippy::unwrap_in_result)]

use crate::parameters::{
    network::error::ParametersBuilderError,
    testnet::{ConfiguredActivationHeights, Parameters, ParametersBuilder, RegtestParameters},
};
use color_eyre::Report;

use super::*;

#[test]
fn nu7_funding_periods_preserve_partial_periods_and_the_third_halving() -> Result<(), Report> {
    let builder = Parameters::build().with_activation_heights(ConfiguredActivationHeights {
        nu7: Some(4_200_000),
        ..Network::new_default_testnet().activation_list().into()
    })?;
    let network = builder.clone().to_network()?;
    for (height, period) in [
        (4_199_999, 136),
        (4_200_000, 136),
        (4_231_000, 136),
        (4_292_999, 136),
        (4_293_000, 137),
        (4_397_999, 137),
        (4_398_000, 138),
    ] {
        assert_eq!(
            funding_stream_address_period(Height(height), &network),
            period
        );
    }
    assert_eq!(height_for_halving(3, &network), Some(Height(5_028_000)));
    for height in [4_476_000, 5_027_999] {
        let height = Height(height);
        let values =
            funding_stream_values(height, &network, scheduled_block_subsidy(height, &network)?)?;
        assert_eq!(
            values[&FundingStreamReceiver::MajorGrants].zatoshis(),
            4_166_666
        );
        assert_eq!(
            values[&FundingStreamReceiver::Deferred].zatoshis(),
            6_249_999
        );
    }
    assert!(network.funding_streams(Height(5_028_000)).is_none());

    // Replacing or removing NU7 must recompute from the original end, not compound a stretch.
    for (activation, end) in [(Some(4_300_002), 4_827_996), (None, 4_476_000)] {
        let network = builder
            .clone()
            .with_activation_heights(ConfiguredActivationHeights {
                nu7: activation,
                ..Network::new_default_testnet().activation_list().into()
            })?
            .to_network()?;
        assert!(network.funding_streams(Height(end).previous()?).is_some());
        assert!(network.funding_streams(Height(end)).is_none());
    }

    // Missing ranges inherit the adjusted end; explicit ranges are never rewritten.
    let inherited = builder
        .clone()
        .with_funding_streams(vec![
            Default::default(),
            Default::default(),
            Default::default(),
        ])
        .to_network()?;
    assert!(inherited.funding_streams(Height(5_027_999)).is_some());
    let explicit = builder
        .with_funding_streams(
            constants::testnet::FUNDING_STREAMS
                .iter()
                .map(Into::into)
                .collect(),
        )
        .to_network()?;
    assert!(explicit.funding_streams(Height(4_476_000)).is_none());

    for activation in [4_476_000, 4_476_003] {
        let network = Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                nu7: Some(activation),
                ..Network::new_default_testnet().activation_list().into()
            })?
            .to_network()?;
        assert!(network.funding_streams(Height(activation)).is_none());
    }
    Ok(())
}

#[test]
fn nu7_funding_addresses_change_at_the_stretched_period_boundary() -> Result<(), Report> {
    let addresses = [
        constants::testnet::FUNDING_STREAM_ECC_ADDRESSES[0],
        constants::testnet::FUNDING_STREAM_ZF_ADDRESSES[0],
    ];
    let streams = FundingStreams::new(
        Height(3_536_500)..Height(5_028_000),
        [(
            FundingStreamReceiver::MajorGrants,
            FundingStreamRecipient::new(8, (0..27).map(|index| addresses[index % 2])),
        )]
        .into_iter()
        .collect(),
    );
    let network = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(4_200_000),
            ..Network::new_default_testnet().activation_list().into()
        })?
        .with_funding_streams(vec![(&streams).into()])
        .to_network()?;
    for (height, expected) in [
        (4_199_999, addresses[1]),
        (4_200_000, addresses[1]),
        (4_292_999, addresses[1]),
        (4_293_000, addresses[0]),
        (4_397_999, addresses[0]),
        (4_398_000, addresses[1]),
    ] {
        assert_eq!(
            funding_stream_address(Height(height), &network, FundingStreamReceiver::MajorGrants),
            Some(&expected.parse()?),
        );
    }
    Ok(())
}

#[test]
fn cumulative_issuance_matches_the_schedule() -> Result<(), Report> {
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
    let scheduled = (0..=200).try_fold(Amount::zero(), |sum, height| {
        Ok::<_, SubsidyError>((sum + scheduled_block_subsidy(Height(height), &network)?)?)
    })?;
    assert_eq!(scheduled.zatoshis(), 83_937_500_000);
    assert_eq!(
        cumulative_scheduled_issuance(Height(200), &network)?,
        scheduled
    );
    Ok(())
}

#[test]
fn halving_intervals_reject_unserializable_and_unsupported_heights() {
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
    let builder = Parameters::build().with_halving_interval(1)?;

    assert_subsidy_error(
        builder.clone(),
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
        assert_subsidy_error(builder, ParametersBuilderError::InvalidSubsidySchedule);
    }
    Ok(())
}

#[test]
fn monetary_validation_preserves_valid_schedules() -> Result<(), Report> {
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
    Ok(())
}

#[test]
fn negative_halving_indices_are_rejected_before_issuance_validation() -> Result<(), Report> {
    let builder = Parameters::build()
        .with_slow_start_interval(Height(20))
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            canopy: Some(1),
            ..Default::default()
        })?
        .with_halving_interval(1)?
        .with_funding_streams(Vec::new());
    assert_subsidy_error(builder, ParametersBuilderError::InvalidHalvingInterval);
    Ok(())
}
/// A valid spacing schedule can require more addresses than the default funding streams supply.
#[test]
fn funding_address_shortages_are_fallible_after_spacing_changes() -> Result<(), Report> {
    let streams = FundingStreams::new(
        Height(1_028_500)..Height(2_796_000),
        [(
            FundingStreamReceiver::MajorGrants,
            FundingStreamRecipient::new(8, [constants::testnet::FUNDING_STREAM_MG_ADDRESSES[0]]),
        )]
        .into_iter()
        .collect(),
    );
    let builder = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(584_000),
            nu7: Some(1_000_000),
            ..Default::default()
        })?
        .with_funding_streams(vec![(&streams).into()]);
    assert!(builder.clone().to_network().is_err());

    // Explicitly extending the recipients repairs the shortage without changing the schedule.
    let network = builder.extend_funding_streams()?.to_network()?;
    assert_eq!(network.height_for_first_halving(), Height(1_348_000));
    let range = Height(1_028_500)..Height(2_796_000);
    assert_eq!(
        crate::parameters::subsidy::funding_stream_address_period(range.end.previous()?, &network)
            - crate::parameters::subsidy::funding_stream_address_period(range.start, &network)
            + 1,
        18,
    );
    Ok(())
}

/// Both builder entry points must reject invalid subsidy parameters.
fn assert_subsidy_error(builder: ParametersBuilder, expected: ParametersBuilderError) {
    assert_eq!(
        builder.clone().extend_funding_streams().unwrap_err(),
        expected,
    );
    assert_eq!(builder.to_network().unwrap_err(), expected);
}

#[test]
fn halving_heights_invert_slow_start_across_spacing_changes() -> Result<(), Report> {
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
