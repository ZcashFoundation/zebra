//! Fixed test vectors for the network consensus parameters.

use zcash_primitives::consensus::NetworkConstants as _;
use zcash_primitives::consensus::{self as zp_consensus, Parameters};

use crate::{
    block::Height,
    parameters::{
        subsidy::{
            FundingStreamReceiver, FUNDING_STREAM_ECC_ADDRESSES_MAINNET,
            FUNDING_STREAM_ECC_ADDRESSES_TESTNET, POST_NU6_FUNDING_STREAMS_TESTNET,
            PRE_NU6_FUNDING_STREAMS_TESTNET,
        },
        testnet::{
            self, ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams, MAX_NETWORK_NAME_LENGTH, RESERVED_NETWORK_NAMES,
        },
        Network, NetworkUpgrade, MAINNET_ACTIVATION_HEIGHTS, TESTNET_ACTIVATION_HEIGHTS,
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
        zp_consensus::NetworkUpgrade::Nu7,
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
            nu7: Some(expected_activation_height),
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

    for nu in NetworkUpgrade::iter().skip(1) {
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
        // TODO: Remove this once the testnet parameters are being serialized (#8920).
        (Height(100), NetworkUpgrade::Nu5),
        (Height(101), NetworkUpgrade::Nu7),
    ];

    for (network, expected_activation_heights) in [
        (Network::Mainnet, MAINNET_ACTIVATION_HEIGHTS),
        (Network::new_default_testnet(), TESTNET_ACTIVATION_HEIGHTS),
        (
            Network::new_regtest(None, None),
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
        Network::new_regtest(None, None).to_string(),
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

    // We expect the first 8 network upgrades to be included, up to and including NU5
    let expected_network_upgrades = NetworkUpgrade::iter().take(8);
    let full_activation_list_network_upgrades: Vec<_> = network
        .full_activation_list()
        .into_iter()
        .map(|(_, nu)| nu)
        .collect();

    for expected_network_upgrade in expected_network_upgrades {
        assert!(
            full_activation_list_network_upgrades.contains(&expected_network_upgrade),
            "full activation list should contain expected network upgrade"
        );
    }
}

/// Tests that a set of constraints are enforced when building Testnet parameters,
/// and that funding stream configurations that should be valid can be built.
#[test]
fn check_configured_funding_stream_constraints() {
    let configured_funding_streams = [
        Default::default(),
        ConfiguredFundingStreams {
            height_range: Some(Height(2_000_000)..Height(2_200_000)),
            ..Default::default()
        },
        ConfiguredFundingStreams {
            height_range: Some(Height(20)..Height(30)),
            recipients: None,
        },
        ConfiguredFundingStreams {
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Ecc,
                numerator: 20,
                addresses: Some(
                    FUNDING_STREAM_ECC_ADDRESSES_TESTNET
                        .map(Into::into)
                        .to_vec(),
                ),
            }]),
            ..Default::default()
        },
        ConfiguredFundingStreams {
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Ecc,
                numerator: 100,
                addresses: Some(
                    FUNDING_STREAM_ECC_ADDRESSES_TESTNET
                        .map(Into::into)
                        .to_vec(),
                ),
            }]),
            ..Default::default()
        },
    ];

    for configured_funding_streams in configured_funding_streams {
        for is_pre_nu6 in [false, true] {
            let (network_funding_streams, default_funding_streams) = if is_pre_nu6 {
                (
                    testnet::Parameters::build()
                        .with_pre_nu6_funding_streams(configured_funding_streams.clone())
                        .to_network()
                        .pre_nu6_funding_streams()
                        .clone(),
                    PRE_NU6_FUNDING_STREAMS_TESTNET.clone(),
                )
            } else {
                (
                    testnet::Parameters::build()
                        .with_post_nu6_funding_streams(configured_funding_streams.clone())
                        .to_network()
                        .post_nu6_funding_streams()
                        .clone(),
                    POST_NU6_FUNDING_STREAMS_TESTNET.clone(),
                )
            };

            let expected_height_range = configured_funding_streams
                .height_range
                .clone()
                .unwrap_or(default_funding_streams.height_range().clone());

            assert_eq!(
                network_funding_streams.height_range().clone(),
                expected_height_range,
                "should use default start height when unconfigured"
            );

            let expected_recipients = configured_funding_streams
                .recipients
                .clone()
                .map(|recipients| {
                    recipients
                        .into_iter()
                        .map(ConfiguredFundingStreamRecipient::into_recipient)
                        .collect()
                })
                .unwrap_or(default_funding_streams.recipients().clone());

            assert_eq!(
                network_funding_streams.recipients().clone(),
                expected_recipients,
                "should use default start height when unconfigured"
            );
        }
    }

    std::panic::set_hook(Box::new(|_| {}));

    // should panic when there are fewer addresses than the max funding stream address index.
    let expected_panic_num_addresses = std::panic::catch_unwind(|| {
        testnet::Parameters::build().with_pre_nu6_funding_streams(ConfiguredFundingStreams {
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Ecc,
                numerator: 10,
                addresses: Some(vec![]),
            }]),
            ..Default::default()
        });
    });

    // should panic when sum of numerators is greater than funding stream denominator.
    let expected_panic_numerator = std::panic::catch_unwind(|| {
        testnet::Parameters::build().with_pre_nu6_funding_streams(ConfiguredFundingStreams {
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Ecc,
                numerator: 101,
                addresses: Some(
                    FUNDING_STREAM_ECC_ADDRESSES_TESTNET
                        .map(Into::into)
                        .to_vec(),
                ),
            }]),
            ..Default::default()
        });
    });

    // should panic when recipient addresses are for Mainnet.
    let expected_panic_wrong_addr_network = std::panic::catch_unwind(|| {
        testnet::Parameters::build().with_pre_nu6_funding_streams(ConfiguredFundingStreams {
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Ecc,
                numerator: 10,
                addresses: Some(
                    FUNDING_STREAM_ECC_ADDRESSES_MAINNET
                        .map(Into::into)
                        .to_vec(),
                ),
            }]),
            ..Default::default()
        });
    });

    // drop panic hook before expecting errors.
    let _ = std::panic::take_hook();

    expected_panic_num_addresses.expect_err("should panic when there are too few addresses");
    expected_panic_numerator.expect_err(
        "should panic when sum of numerators is greater than funding stream denominator",
    );
    expected_panic_wrong_addr_network
        .expect_err("should panic when recipient addresses are for Mainnet");
}
