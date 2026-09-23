use chrono::{Duration, TimeZone, Utc};

use crate::{
    block,
    chain_tip::{
        mock::MockChainTip, ChainTip, NetworkChainTipHeightEstimator, AT_OR_NEAR_TIP_THRESHOLD,
    },
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        Network, NetworkUpgrade,
    },
};

/// Check that the at-or-near-tip threshold is inclusive.
#[test]
fn at_or_near_network_tip_threshold_is_inclusive() {
    let network = Network::Mainnet;
    let (chain_tip, mock_chain_tip_sender) = MockChainTip::new();

    mock_chain_tip_sender.send_best_tip_height(block::Height(2_500_000));
    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(AT_OR_NEAR_TIP_THRESHOLD);

    assert!(chain_tip.is_at_or_near_network_tip(&network));

    mock_chain_tip_sender
        .send_estimated_distance_to_network_chain_tip(AT_OR_NEAR_TIP_THRESHOLD + 1);

    assert!(!chain_tip.is_at_or_near_network_tip(&network));
}

/// A spacing change applies to the interval ending at the activation block.
#[test]
fn spacing_transition_intervals_use_candidate_height() {
    let network = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(100),
            nu7: Some(200),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");
    let start_time = Utc.timestamp_opt(1_600_000_000, 0).unwrap();

    for upgrade in [NetworkUpgrade::Blossom, NetworkUpgrade::Nu7] {
        let activation = upgrade.activation_height(&network).unwrap();
        let parent = (activation - 1).unwrap();
        let old_spacing = NetworkUpgrade::target_spacing_for_height(&network, parent).num_seconds();
        let new_spacing = upgrade.target_spacing().num_seconds();

        for (start_height, elapsed, expected_height) in [
            (
                (parent - 1).unwrap(),
                old_spacing - 1,
                (parent - 1).unwrap(),
            ),
            ((parent - 1).unwrap(), old_spacing, parent),
            ((parent - 1).unwrap(), old_spacing + new_spacing - 1, parent),
            ((parent - 1).unwrap(), old_spacing + new_spacing, activation),
            (parent, new_spacing - 1, parent),
            (parent, new_spacing, activation),
            (parent, 3 * new_spacing, (activation + 2).unwrap()),
            (activation, 0, activation),
            (activation, new_spacing, (activation + 1).unwrap()),
        ] {
            assert_eq!(
                NetworkChainTipHeightEstimator::new(start_time, start_height, &network)
                    .estimate_height_at(start_time + Duration::seconds(elapsed)),
                expected_height,
                "{upgrade:?}, starting at {start_height:?}, after {elapsed} seconds",
            );
        }
    }
}

/// Exact and fractional past times floor to the preceding height without an extra block.
#[test]
fn height_estimates_floor_time_and_saturate() {
    let network = Network::Mainnet;
    let start_time = Utc.timestamp_opt(1_600_000_000, 0).unwrap();
    let start_height = block::Height(1_000_000);

    for (nanoseconds, height_difference) in [
        (-75_000_000_001, -2),
        (-75_000_000_000, -1),
        (-1, -1),
        (0, 0),
        (74_999_999_999, 0),
        (75_000_000_000, 1),
    ] {
        assert_eq!(
            NetworkChainTipHeightEstimator::new(start_time, start_height, &network)
                .estimate_height_at(start_time + Duration::nanoseconds(nanoseconds)),
            (start_height + height_difference).unwrap(),
        );
    }

    for (height, offset) in [
        (block::Height(0), Duration::nanoseconds(-1)),
        (block::Height::MAX, Duration::days(1)),
    ] {
        assert_eq!(
            NetworkChainTipHeightEstimator::new(start_time, height, &network)
                .estimate_height_at(start_time + offset),
            height,
        );
    }
}
