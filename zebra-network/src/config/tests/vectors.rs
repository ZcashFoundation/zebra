//! Fixed test vectors for zebra-network configuration.

use static_assertions::const_assert;
use zebra_chain::{
    block::Height,
    parameters::{
        constants::magics,
        subsidy::FundingStreamReceiver,
        testnet::{self, ConfiguredFundingStreams},
        Magic, Network, NetworkUpgrade,
    },
};

use crate::{
    constants::{INBOUND_PEER_LIMIT_MULTIPLIER, OUTBOUND_PEER_LIMIT_MULTIPLIER},
    Config,
};

#[test]
fn parse_config_listen_addr() {
    let _init_guard = zebra_test::init();

    let fixtures = vec![
        ("listen_addr = '0.0.0.0'", "0.0.0.0:8233"),
        ("listen_addr = '0.0.0.0:9999'", "0.0.0.0:9999"),
        (
            "listen_addr = '0.0.0.0'\nnetwork = 'Testnet'",
            "0.0.0.0:18233",
        ),
        (
            "listen_addr = '0.0.0.0:8233'\nnetwork = 'Testnet'",
            "0.0.0.0:8233",
        ),
        ("listen_addr = '[::]'", "[::]:8233"),
        ("listen_addr = '[::]:9999'", "[::]:9999"),
        ("listen_addr = '[::]'\nnetwork = 'Testnet'", "[::]:18233"),
        (
            "listen_addr = '[::]:8233'\nnetwork = 'Testnet'",
            "[::]:8233",
        ),
        ("listen_addr = '[::1]:8233'", "[::1]:8233"),
        ("listen_addr = '[2001:db8::1]:8233'", "[2001:db8::1]:8233"),
    ];

    for (config, value) in fixtures {
        let config: Config = toml::from_str(config).unwrap();
        assert_eq!(config.listen_addr.to_string(), value);
    }
}

/// Make sure the peer connection limits are consistent with each other.
#[test]
fn ensure_peer_connection_limits_consistent() {
    let _init_guard = zebra_test::init();

    // Zebra should allow more inbound connections, to avoid connection exhaustion
    const_assert!(INBOUND_PEER_LIMIT_MULTIPLIER > OUTBOUND_PEER_LIMIT_MULTIPLIER);

    let config = Config::default();

    assert!(
        config.peerset_inbound_connection_limit() - config.peerset_outbound_connection_limit()
            >= 50,
        "default config should allow more inbound connections, to avoid connection exhaustion",
    );
}

#[test]
fn testnet_params_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: testnet::Parameters::build()
            .with_network_magic(Magic([0; 4]))
            .unwrap()
            .with_disable_pow(true)
            .to_network()
            .expect("failed to build configured network"),
        initial_testnet_peers: [].into(),
        ..Config::default()
    };

    let serialized = toml::to_string(&config).unwrap();
    let deserialized: Config = toml::from_str(&serialized).unwrap();

    assert_eq!(config, deserialized);
}

#[test]
fn default_config_uses_ipv6() {
    let _init_guard = zebra_test::init();
    let config = Config::default();

    assert_eq!(config.listen_addr.to_string(), "[::]:8233");
    assert!(config.listen_addr.is_ipv6());
}

#[test]
fn funding_streams_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let fs = testnet::Parameters::default()
        .funding_streams()
        .iter()
        .map(ConfiguredFundingStreams::from)
        .collect();

    let config = Config {
        network: testnet::Parameters::build()
            .with_funding_streams(fs)
            .to_network()
            .expect("failed to build configured network"),
        initial_testnet_peers: [].into(),
        ..Config::default()
    };

    let serialized = toml::to_string(&config).unwrap();
    let deserialized: Config = toml::from_str(&serialized).unwrap();

    assert_eq!(config, deserialized);
}

#[test]
fn empty_funding_streams_survive_configuration_roundtrip() {
    let _init_guard = zebra_test::init();
    let config = Config {
        network: testnet::Parameters::build()
            .with_network_magic(Magic([0; 4]))
            .unwrap()
            .with_funding_streams(Vec::new())
            .to_network()
            .unwrap(),
        initial_testnet_peers: [].into(),
        ..Config::default()
    };
    let deserialized: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    let Network::Testnet(params) = deserialized.network else {
        panic!("configured Testnet must stay a Testnet");
    };
    assert!(params.funding_streams().is_empty());

    let omitted: Config =
        toml::from_str("network = 'Testnet'\n[testnet_parameters]\ncheckpoints = true\n").unwrap();
    let Network::Testnet(params) = omitted.network else {
        panic!("configured Testnet must stay a Testnet");
    };
    assert_eq!(
        params.funding_streams(),
        testnet::Parameters::default().funding_streams(),
    );
}

#[test]
fn funding_stream_extension_rejects_zero_address_period() {
    let _init_guard = zebra_test::init();
    let config = r#"
network = "Testnet"
initial_testnet_peers = []
[testnet_parameters]
network_magic = [0, 0, 0, 0]
checkpoints = true
pre_blossom_halving_interval = 1
extend_funding_stream_addresses_as_required = true
"#;
    assert!(toml::from_str::<Config>(config).is_err());
}

/// Checks that a configured Testnet's temporary Orchard-disabling soft fork height
/// survives a serialization round-trip.
#[test]
fn temporary_orchard_disabling_soft_fork_height_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let soft_fork_height = Height(2_000_000);

    let config = Config {
        network: testnet::Parameters::build()
            .with_network_magic(Magic([0; 4]))
            .unwrap()
            .with_temporary_orchard_disabling_soft_fork_height(soft_fork_height)
            .to_network()
            .expect("failed to build configured network"),
        initial_testnet_peers: [].into(),
        ..Config::default()
    };

    let serialized = toml::to_string(&config).unwrap();
    let deserialized: Config = toml::from_str(&serialized).unwrap();

    assert_eq!(config, deserialized);

    // The configured height must be preserved through the round-trip.
    let Network::Testnet(params) = &deserialized.network else {
        panic!("deserialized network must be a Testnet");
    };
    assert_eq!(
        params.temporary_orchard_disabling_soft_fork_height(),
        Some(soft_fork_height),
    );
}

/// Coalesced upgrades must not acquire earlier Regtest defaults after serialization.
#[test]
fn coincident_regtest_upgrades_preserve_activation_on_roundtrip() {
    let _init_guard = zebra_test::init();
    let config: Config = toml::from_str(
        "network = 'Regtest'\n\
         [testnet_parameters.activation_heights]\n\
         Overwinter = 10\n\
         NU7 = 10\n",
    )
    .unwrap();
    let restored: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();

    for network in [&config.network, &restored.network] {
        assert_eq!(
            NetworkUpgrade::current(network, Height(9)),
            NetworkUpgrade::Genesis
        );
        assert_eq!(
            NetworkUpgrade::current(network, Height(10)),
            NetworkUpgrade::Nu7
        );
    }
    assert_eq!(config, restored);
}

/// Checks that a Regtest configured to forbid unshielded coinbase spends survives a
/// serialization round-trip, and that the flag does not change the network's identity.
#[test]
fn regtest_should_allow_unshielded_coinbase_spends_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: Network::new_regtest(testnet::RegtestParameters {
            should_allow_unshielded_coinbase_spends: Some(false),
            ..Default::default()
        }),
        initial_testnet_peers: [].into(),
        ..Config::default()
    };

    // Forbidding unshielded coinbase spends must not stop the network from being Regtest.
    assert!(config.network.is_regtest());

    let serialized = toml::to_string(&config).unwrap();
    let deserialized: Config = toml::from_str(&serialized).unwrap();

    assert_eq!(config, deserialized);
    assert!(deserialized.network.is_regtest());

    let Network::Testnet(params) = &deserialized.network else {
        panic!("deserialized network must be Regtest");
    };
    assert!(!params.should_allow_unshielded_coinbase_spends());
}

/// Checks that the Regtest-only `should_allow_unshielded_coinbase_spends` knob is rejected
/// on a configured Testnet rather than silently ignored.
#[test]
fn should_allow_unshielded_coinbase_spends_rejected_on_testnet() {
    let _init_guard = zebra_test::init();

    let toml = "network = 'Testnet'\n\n[testnet_parameters]\nshould_allow_unshielded_coinbase_spends = true\n";
    let err = toml::from_str::<Config>(toml)
        .expect_err("configured Testnet must reject the Regtest-only field");

    assert!(
        err.to_string()
            .contains("should_allow_unshielded_coinbase_spends"),
        "unexpected error: {err}"
    );
}

/// A configured reissuance height must survive serialization and reject invalid boundaries.
#[test]
fn nsm_reissuance_configuration_is_validated() {
    let _init_guard = zebra_test::init();
    let configuration = |height| {
        format!(
            "network = 'Regtest'\n\
             [testnet_parameters]\n\
             nsm_reissuance_height = {height}\n\
             [testnet_parameters.activation_heights]\n\
             NU7 = 9\n"
        )
    };
    let config: Config = toml::from_str(&configuration(12)).unwrap();
    let config: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    let reserve = zebra_chain::amount::Amount::try_from(100_000_000).unwrap();
    assert_eq!(
        zebra_chain::parameters::subsidy::nsm_subsidy(Height(11), &config.network, reserve)
            .unwrap()
            .zatoshis(),
        0,
    );
    assert!(
        zebra_chain::parameters::subsidy::nsm_subsidy(Height(12), &config.network, reserve)
            .unwrap()
            .zatoshis()
            > 0
    );

    for height in [0, 8, u32::MAX] {
        assert!(toml::from_str::<Config>(&configuration(height)).is_err());
    }
    assert!(toml::from_str::<Config>(
        "network = 'Regtest'\n[testnet_parameters]\nnsm_reissuance_height = 12\n"
    )
    .is_err());
}

#[test]
fn incompatible_testnet_requires_isolated_magic() {
    let _init_guard = zebra_test::init();
    let uppercase_seed = Config::default()
        .initial_testnet_peers
        .first()
        .unwrap()
        .to_ascii_uppercase();

    // None of these peer lists can guarantee isolation from public Testnet.
    for peers in [format!("{uppercase_seed:?}"), String::new()] {
        let config = format!(
            "network = 'Testnet'\n\
             initial_testnet_peers = [{peers}]\n\
             [testnet_parameters]\n\
             checkpoints = true\n\
             temporary_orchard_disabling_soft_fork_height = 2000000\n"
        );
        assert!(toml::from_str::<Config>(&config).is_err());
        assert!(toml::from_str::<Config>(&format!(
            "{config}network_magic = {:?}\n",
            magics::TESTNET.0,
        ))
        .is_err());
    }

    let private: Config = toml::from_str(
        "network = 'Testnet'\n\
         initial_testnet_peers = ['127.0.0.1:18233']\n\
         [testnet_parameters]\n\
         checkpoints = true\n\
         network_magic = [0, 0, 0, 0]\n\
         temporary_orchard_disabling_soft_fork_height = 2000000\n",
    )
    .unwrap();
    assert_eq!(private.network.magic(), Magic([0; 4]));
    let Network::Testnet(params) = private.network else {
        panic!("configured Testnet must stay a Testnet");
    };
    assert_eq!(
        params.temporary_orchard_disabling_soft_fork_height(),
        Some(Height(2_000_000)),
    );

    let public: Config = toml::from_str(&format!(
        "network = 'Testnet'\n\
         initial_testnet_peers = [{uppercase_seed:?}]\n\
         [testnet_parameters]\n\
         checkpoints = true\n\
         network_magic = {:?}\n",
        magics::TESTNET.0,
    ))
    .unwrap();
    assert_eq!(public.network, Network::new_default_testnet());
}

#[test]
fn empty_funding_streams_reject_legacy_declarations() {
    let _init_guard = zebra_test::init();

    for network in ["Testnet", "Regtest"] {
        let checkpoints = network == "Testnet";
        for legacy_field in ["pre_nu6_funding_streams", "post_nu6_funding_streams"] {
            let config = format!(
                "network = '{network}'\n\
                 initial_testnet_peers = []\n\
                 [testnet_parameters]\n\
                 checkpoints = {checkpoints}\n\
                 network_magic = [0, 0, 0, 0]\n\
                 funding_streams = []\n\
                 {legacy_field} = {{ height_range = {{ start = 1, end = 2 }}, recipients = [{{ receiver = 'Deferred', numerator = 1 }}] }}\n"
            );
            assert!(toml::from_str::<Config>(&config).is_err());

            // Omitting the new field must retain the requested legacy payout.
            let config: Config =
                toml::from_str(&config.replace("funding_streams = []\n", "")).unwrap();
            assert_eq!(
                config
                    .network
                    .funding_streams(Height(1))
                    .unwrap()
                    .recipients()[&FundingStreamReceiver::Deferred]
                    .numerator(),
                1,
            );
        }
    }
}

#[test]
fn legacy_funding_streams_keep_precedence_over_nonempty_lists() {
    let _init_guard = zebra_test::init();

    for network in ["Testnet", "Regtest"] {
        let checkpoints = network == "Testnet";
        let config: Config = toml::from_str(&format!(
            "network = '{network}'\n\
             initial_testnet_peers = []\n\
             [testnet_parameters]\n\
             checkpoints = {checkpoints}\n\
             network_magic = [0, 0, 0, 0]\n\
             pre_nu6_funding_streams = {{ height_range = {{ start = 1, end = 4 }}, recipients = [{{ receiver = 'Deferred', numerator = 1 }}] }}\n\
             post_nu6_funding_streams = {{ height_range = {{ start = 2, end = 5 }}, recipients = [{{ receiver = 'Deferred', numerator = 2 }}] }}\n\
             funding_streams = [{{ height_range = {{ start = 3, end = 6 }}, recipients = [{{ receiver = 'Deferred', numerator = 3 }}] }}]\n"
        ))
        .unwrap();

        for (height, numerator) in [(3, 1), (4, 2), (5, 3)] {
            assert_eq!(
                config
                    .network
                    .funding_streams(Height(height))
                    .unwrap()
                    .recipients()[&FundingStreamReceiver::Deferred]
                    .numerator(),
                numerator,
            );
        }
    }
}
