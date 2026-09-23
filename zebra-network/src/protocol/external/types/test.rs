//! Network protocol version boundary tests.

use super::*;

#[test]
fn version_extremes_mainnet() {
    version_extremes(&Mainnet)
}

#[test]
fn version_extremes_testnet() {
    version_extremes(&Network::new_default_testnet())
}

#[test]
fn nu7_peer_versions_support_configured_activation() {
    use zebra_chain::parameters::testnet::{ConfiguredActivationHeights, RegtestParameters};

    let activation_height = block::Height(10);
    let regtest = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu7: Some(activation_height.0),
            ..Default::default()
        },
        ..Default::default()
    });

    for (network, minimum) in [
        (Mainnet, Version(170_190)),
        (Network::new_default_testnet(), Version(170_180)),
        (regtest.clone(), Version(170_180)),
    ] {
        assert_eq!(Version::min_specified_for_upgrade(&network, Nu7), minimum);
        assert!(CURRENT_NETWORK_PROTOCOL_VERSION >= minimum);
    }

    assert_eq!(NetworkUpgrade::current(&regtest, activation_height), Nu7);
    assert_eq!(
        Version::min_remote_for_height(&regtest, activation_height),
        Version(170_180),
    );
    assert!(
        Version::min_remote_for_height(&regtest, block::Height(activation_height.0 - 1))
            < Version(170_180)
    );
}

/// Test the min_specified_for_upgrade and min_specified_for_height functions for `network` with
/// extreme values.
fn version_extremes(network: &Network) {
    let _init_guard = zebra_test::init();

    assert_eq!(
        Version::min_specified_for_height(network, block::Height(0)),
        Version::min_specified_for_upgrade(network, BeforeOverwinter),
    );

    // We assume that the last version we know about continues forever
    // (even if we suspect that won't be true)
    assert_ne!(
        Version::min_specified_for_height(network, block::Height::MAX),
        Version::min_specified_for_upgrade(network, BeforeOverwinter),
    );
}

#[test]
fn version_consistent_mainnet() {
    version_consistent(&Mainnet)
}

#[test]
fn version_consistent_testnet() {
    version_consistent(&Network::new_default_testnet())
}

/// Check that the min_specified_for_upgrade and min_specified_for_height functions
/// are consistent for `network`.
fn version_consistent(network: &Network) {
    let _init_guard = zebra_test::init();

    let highest_network_upgrade = NetworkUpgrade::current(network, block::Height::MAX);
    assert!(
        matches!(highest_network_upgrade, Nu6 | Nu6_1 | Nu6_2 | Nu6_3 | Nu7),
        "expected coverage of all network upgrades: \
        add the new network upgrade to the list in this test"
    );

    for &network_upgrade in &[
        BeforeOverwinter,
        Overwinter,
        Sapling,
        Blossom,
        Heartwood,
        Canopy,
        Nu5,
        Nu6,
        Nu6_1,
        Nu6_2,
        Nu6_3,
        Nu7,
    ] {
        let height = network_upgrade.activation_height(network);
        if let Some(height) = height {
            assert_eq!(
                Version::min_specified_for_upgrade(network, network_upgrade),
                Version::min_specified_for_height(network, height)
            );
        }
    }
}
