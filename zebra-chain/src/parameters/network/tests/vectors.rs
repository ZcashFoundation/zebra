//! Fixed test vectors for the network consensus parameters.

use zcash_primitives::consensus::{self as zp_consensus, Parameters};
use zcash_protocol::consensus::NetworkConstants as _;

use crate::{
    block::Height,
    parameters::{
        testnet::{
            self, ConfiguredActivationHeights, MAX_NETWORK_NAME_LENGTH, RESERVED_NETWORK_NAMES,
        },
        Network, NetworkUpgrade, MAINNET_ACTIVATION_HEIGHTS, NETWORK_UPGRADES_IN_ORDER,
        TESTNET_ACTIVATION_HEIGHTS,
    },
};

/// Checks that every method in the `Parameters` impl for `zebra_chain::Network` has the same output
/// as the Parameters impl for `zcash_primitives::consensus::Network` on Mainnet and the default Testnet.
#[test]
fn check_parameters_impl() {
    let zp_network_upgrades = [
        zp_consensus::NetworkUpgrade::Overwinter,
        zp_consensus::NetworkUpgrade::Sapling,
        zp_consensus::NetworkUpgrade::Blossom,
        zp_consensus::NetworkUpgrade::Heartwood,
        zp_consensus::NetworkUpgrade::Canopy,
        zp_consensus::NetworkUpgrade::Nu5,
    ];

    for (network, zp_network) in [
        (Network::Mainnet, zp_consensus::Network::MainNetwork),
        (
            Network::new_default_testnet(),
            zp_consensus::Network::TestNetwork,
        ),
    ] {
        for nu in zp_network_upgrades {
            let activation_height = network
                .activation_height(nu)
                .expect("must have activation height for past network upgrades");

            assert_eq!(
                activation_height,
                zp_network
                    .activation_height(nu)
                    .expect("must have activation height for past network upgrades"),
                "Parameters::activation_heights() outputs must match"
            );

            let activation_height: u32 = activation_height.into();

            for height in (activation_height - 1)..=(activation_height + 1) {
                for nu in zp_network_upgrades {
                    let height = zp_consensus::BlockHeight::from_u32(height);
                    assert_eq!(
                        network.is_nu_active(nu, height),
                        zp_network.is_nu_active(nu, height),
                        "Parameters::is_nu_active() outputs must match",
                    );
                }
            }
        }

        assert_eq!(
            network.coin_type(),
            zp_network.coin_type(),
            "Parameters::coin_type() outputs must match"
        );
        assert_eq!(
            network.hrp_sapling_extended_spending_key(),
            zp_network.hrp_sapling_extended_spending_key(),
            "Parameters::hrp_sapling_extended_spending_key() outputs must match"
        );
        assert_eq!(
            network.hrp_sapling_extended_full_viewing_key(),
            zp_network.hrp_sapling_extended_full_viewing_key(),
            "Parameters::hrp_sapling_extended_full_viewing_key() outputs must match"
        );
        assert_eq!(
            network.hrp_sapling_payment_address(),
            zp_network.hrp_sapling_payment_address(),
            "Parameters::hrp_sapling_payment_address() outputs must match"
        );
        assert_eq!(
            network.b58_pubkey_address_prefix(),
            zp_network.b58_pubkey_address_prefix(),
            "Parameters::b58_pubkey_address_prefix() outputs must match"
        );
        assert_eq!(
            network.b58_script_address_prefix(),
            zp_network.b58_script_address_prefix(),
            "Parameters::b58_script_address_prefix() outputs must match"
        );
    }
}

/// Checks that `NetworkUpgrade::activation_height()` returns the activation height of the next
/// network upgrade if it doesn't find an activation height for a prior network upgrade, that the
/// `Genesis` upgrade is always at `Height(0)`, and that the default Mainnet/Testnet/Regtest activation
/// heights are what's expected.
#[test]
fn activates_network_upgrades_correctly() {
    let expected_activation_height = 1;
    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(expected_activation_height),
            ..Default::default()
        })
        .to_network();

    let genesis_activation_height = NetworkUpgrade::Genesis
        .activation_height(&network)
        .expect("must return an activation height");

    assert_eq!(
        genesis_activation_height,
        Height(0),
        "activation height for all networks after Genesis and BeforeOverwinter should match NU5 activation height"
    );

    for nu in NETWORK_UPGRADES_IN_ORDER.into_iter().skip(1) {
        let activation_height = nu
            .activation_height(&network)
            .expect("must return an activation height");

        assert_eq!(
            activation_height, Height(expected_activation_height),
            "activation height for all networks after Genesis and BeforeOverwinter \
            should match NU5 activation height, network_upgrade: {nu}, activation_height: {activation_height:?}"
        );
    }

    let expected_default_regtest_activation_heights = &[
        (Height(0), NetworkUpgrade::Genesis),
        (Height(1), NetworkUpgrade::Canopy),
        // TODO: Remove this once the testnet parameters are being serialized.
        (Height(100), NetworkUpgrade::Nu5),
    ];

    for (network, expected_activation_heights) in [
        (Network::Mainnet, MAINNET_ACTIVATION_HEIGHTS),
        (Network::new_default_testnet(), TESTNET_ACTIVATION_HEIGHTS),
        (
            Network::new_regtest(None),
            expected_default_regtest_activation_heights,
        ),
    ] {
        assert_eq!(
            network.activation_list(),
            expected_activation_heights.iter().cloned().collect(),
            "network activation list should match expected activation heights"
        );
    }
}

/// Checks that configured testnet names are validated and used correctly.
#[test]
fn check_configured_network_name() {
    // Sets a no-op panic hook to avoid long output.
    std::panic::set_hook(Box::new(|_| {}));

    // Checks that reserved network names cannot be used for configured testnets.
    for reserved_network_name in RESERVED_NETWORK_NAMES {
        std::panic::catch_unwind(|| {
            testnet::Parameters::build().with_network_name(reserved_network_name)
        })
        .expect_err("should panic when attempting to set network name as a reserved name");
    }

    // Check that max length is enforced, and that network names may only contain alphanumeric characters and '_'.
    for invalid_network_name in [
        "a".repeat(MAX_NETWORK_NAME_LENGTH + 1),
        "!!!!non-alphanumeric-name".to_string(),
    ] {
        std::panic::catch_unwind(|| {
            testnet::Parameters::build().with_network_name(invalid_network_name)
        })
        .expect_err("should panic when setting network name that's too long or contains non-alphanumeric characters (except '_')");
    }

    drop(std::panic::take_hook());

    // Checks that network names are displayed correctly
    assert_eq!(
        Network::new_default_testnet().to_string(),
        "Testnet",
        "default testnet should be displayed as 'Testnet'"
    );
    assert_eq!(
        Network::Mainnet.to_string(),
        "Mainnet",
        "Mainnet should be displayed as 'Mainnet'"
    );
    assert_eq!(
        Network::new_regtest(None).to_string(),
        "Regtest",
        "Regtest should be displayed as 'Regtest'"
    );

    // Check that network name can contain alphanumeric characters and '_'.
    let expected_name = "ConfiguredTestnet_1";
    let network = testnet::Parameters::build()
        // Check that network name can contain `MAX_NETWORK_NAME_LENGTH` characters
        .with_network_name("a".repeat(MAX_NETWORK_NAME_LENGTH))
        .with_network_name(expected_name)
        .to_network();

    // Check that configured network name is displayed
    assert_eq!(
        network.to_string(),
        expected_name,
        "network must be displayed as configured network name"
    );
}

/// Checks that configured testnet names are validated and used correctly.
#[test]
fn check_network_name() {
    // Sets a no-op panic hook to avoid long output.
    std::panic::set_hook(Box::new(|_| {}));

    // Checks that reserved network names cannot be used for configured testnets.
    for reserved_network_name in RESERVED_NETWORK_NAMES {
        std::panic::catch_unwind(|| {
            testnet::Parameters::build().with_network_name(reserved_network_name)
        })
        .expect_err("should panic when attempting to set network name as a reserved name");
    }

    // Check that max length is enforced, and that network names may only contain alphanumeric characters and '_'.
    for invalid_network_name in [
        "a".repeat(MAX_NETWORK_NAME_LENGTH + 1),
        "!!!!non-alphanumeric-name".to_string(),
    ] {
        std::panic::catch_unwind(|| {
            testnet::Parameters::build().with_network_name(invalid_network_name)
        })
        .expect_err("should panic when setting network name that's too long or contains non-alphanumeric characters (except '_')");
    }

    // Restore the regular panic hook for any unexpected panics
    drop(std::panic::take_hook());

    // Checks that network names are displayed correctly
    assert_eq!(
        Network::new_default_testnet().to_string(),
        "Testnet",
        "default testnet should be displayed as 'Testnet'"
    );
    assert_eq!(
        Network::Mainnet.to_string(),
        "Mainnet",
        "Mainnet should be displayed as 'Mainnet'"
    );

    // TODO: Check Regtest

    // Check that network name can contain alphanumeric characters and '_'.
    let expected_name = "ConfiguredTestnet_1";
    let network = testnet::Parameters::build()
        // Check that network name can contain `MAX_NETWORK_NAME_LENGTH` characters
        .with_network_name("a".repeat(MAX_NETWORK_NAME_LENGTH))
        .with_network_name(expected_name)
        .to_network();

    // Check that configured network name is displayed
    assert_eq!(
        network.to_string(),
        expected_name,
        "network must be displayed as configured network name"
    );
}

#[test]
fn check_full_activation_list() {
    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        })
        .to_network();

    // We expect the first 8 network upgrades to be included, up to NU5
    let expected_network_upgrades = &NETWORK_UPGRADES_IN_ORDER[..8];
    let full_activation_list_network_upgrades: Vec<_> = network
        .full_activation_list()
        .into_iter()
        .map(|(_, nu)| nu)
        .collect();

    for expected_network_upgrade in expected_network_upgrades {
        assert!(
            full_activation_list_network_upgrades.contains(expected_network_upgrade),
            "full activation list should contain expected network upgrade"
        );
    }
}
