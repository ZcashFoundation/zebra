//! Fixed test vectors for zebra-network configuration.

use std::sync::Arc;

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
    types::PeerServices,
    Config, ObjectIndex, ObjectSource,
};

/// Limited service and artifact serving are version 2 flags, and early block relay is a
/// legacy-only experiment.
#[test]
fn advertised_services_depend_on_the_protocol() {
    let config = Config {
        experimental_early_block_relay: true,
        ..Config::default()
    };
    let pruned = PeerServices::NODE_NETWORK_LIMITED;
    assert_eq!(
        config.advertised_services(pruned),
        PeerServices::NODE_EARLY_BLOCK_RELAY
    );
    assert_eq!(
        config.v2_advertised_services(pruned),
        PeerServices::NODE_NETWORK_LIMITED
    );
    let serving = PeerServices::NODE_NETWORK_LIMITED | PeerServices::NODE_SYNC_ARTIFACTS;
    assert_eq!(
        config.advertised_services(serving),
        PeerServices::NODE_EARLY_BLOCK_RELAY
    );
    assert_eq!(config.v2_advertised_services(serving), serving);

    let archive = Config::default();
    assert_eq!(
        archive.advertised_services(PeerServices::NODE_NETWORK),
        PeerServices::NODE_NETWORK
    );
    assert_eq!(
        archive.v2_advertised_services(PeerServices::NODE_NETWORK),
        PeerServices::NODE_NETWORK
    );
    let archive_serving = PeerServices::NODE_NETWORK | PeerServices::NODE_SYNC_ARTIFACTS;
    assert_eq!(
        archive.advertised_services(archive_serving),
        PeerServices::NODE_NETWORK
    );
    assert_eq!(
        archive.v2_advertised_services(archive_serving),
        archive_serving
    );

    // A node with an object source serves `get-object`, so version 2 peers see the bit.
    let (_sender, source) = ObjectSource::channel(Arc::new(ObjectIndex::new()));
    let source_config = Config {
        object_source: Some(source),
        ..Config::default()
    };
    assert_eq!(
        source_config.v2_advertised_services(PeerServices::NODE_NETWORK),
        archive_serving
    );
    assert_eq!(
        source_config.advertised_services(PeerServices::NODE_NETWORK),
        PeerServices::NODE_NETWORK
    );
}

/// The objects a node serves are runtime-only: a config never writes them, a config file that
/// names them is rejected, and two configs are equal only if they watch the same object index.
#[test]
fn object_source_is_runtime_only() {
    let _init_guard = zebra_test::init();

    let (_sender, source) = ObjectSource::channel(Arc::new(ObjectIndex::new()));
    let config = Config {
        object_source: Some(source),
        ..Config::default()
    };

    let written = toml::to_string(&config).expect("configs serialize");
    assert!(
        !written.contains("object_source"),
        "the served objects are not written: {written}"
    );
    let read: Config = toml::from_str(&written).expect("written configs parse");
    assert_eq!(read.object_source, None);
    assert!(
        toml::from_str::<Config>("object_source = 1").is_err(),
        "config files cannot name served objects"
    );

    assert_eq!(config.clone(), config, "a clone watches the same index");
    let (_other_sender, other) = ObjectSource::channel(Arc::new(ObjectIndex::new()));
    assert_ne!(
        Config {
            object_source: Some(other),
            ..Config::default()
        },
        config,
        "a different index"
    );
}

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

/// The peer set's total connection limit has room for the version 2 connections when they are
/// enabled, and is unchanged when they are not.
#[test]
fn total_connection_limit_includes_v2_share_only_when_enabled() {
    let _init_guard = zebra_test::init();

    let legacy = Config::default();
    assert_eq!(legacy.v2_outbound_connection_limit(), 0);
    assert_eq!(legacy.v2_inbound_connection_limit(), 0);
    assert_eq!(
        legacy.peerset_total_connection_limit(),
        legacy.peerset_outbound_connection_limit() + legacy.peerset_inbound_connection_limit(),
    );

    let v2 = Config {
        v2_listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        ..Config::default()
    };
    assert_eq!(
        v2.v2_outbound_connection_limit(),
        v2.peerset_outbound_connection_limit() / 2
    );
    assert_eq!(
        v2.v2_inbound_connection_limit(),
        v2.peerset_inbound_connection_limit() / 2
    );
    assert_eq!(
        v2.peerset_total_connection_limit(),
        v2.peerset_outbound_connection_limit()
            + v2.peerset_inbound_connection_limit()
            + v2.v2_outbound_connection_limit()
            + v2.v2_inbound_connection_limit(),
    );

    assert_eq!(legacy.peerset_initial_target_size, 25);
    assert_eq!(legacy.peerset_outbound_connection_limit(), 75);
    assert_eq!(legacy.peerset_inbound_connection_limit(), 125);
    assert_eq!(
        legacy.peer_addr_response_limit(),
        crate::constants::PEER_ADDR_RESPONSE_LIMIT
    );
    assert_eq!(crate::constants::PEER_ADDR_RESPONSE_LIMIT, 37);

    let reduced_config = Config {
        peerset_initial_target_size: 1,
        ..legacy
    };
    assert_eq!(reduced_config.peerset_outbound_connection_limit(), 3);
    assert_eq!(reduced_config.peer_addr_response_limit(), 1);

    // A configured endpoint gets at least one connection of each kind, even with tiny limits.
    let tiny = Config {
        peerset_initial_target_size: 0,
        ..v2
    };
    assert_eq!(tiny.v2_outbound_connection_limit(), 1);
    assert_eq!(tiny.v2_inbound_connection_limit(), 1);
    assert_eq!(tiny.peerset_total_connection_limit(), 2);
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
            .with_network_magic(Magic([0; 4]))
            .unwrap()
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

/// Invalid operator input must be rejected during loading, not panic then or in subsidy consumers.
#[test]
fn invalid_funding_and_lockbox_configuration_returns_errors() {
    let _init_guard = zebra_test::init();
    let cases = [
        (
            "reversed range",
            r#"funding_streams = [{ height_range = { start = 2, end = 1 }, recipients = [] }]"#,
            "end height",
        ),
        (
            "excessive numerator",
            r#"funding_streams = [{ height_range = { start = 1, end = 2 }, recipients = [{ receiver = "Deferred", numerator = 101 }] }]"#,
            "numerators",
        ),
        (
            "overflowing numerators",
            r#"funding_streams = [{ height_range = { start = 1, end = 2 }, recipients = [{ receiver = "ECC", numerator = 9223372036854775807 }, { receiver = "ZcashFoundation", numerator = 9223372036854775807 }, { receiver = "MajorGrants", numerator = 9223372036854775807 }] }]"#,
            "numerators",
        ),
        (
            "malformed funding address",
            r#"funding_streams = [{ height_range = { start = 1, end = 2 }, recipients = [{ receiver = "ECC", numerator = 7, addresses = ["invalid"] }] }]"#,
            "funding stream address",
        ),
        (
            "missing default height range",
            r#"funding_streams = [{ height_range = { start = 0, end = 0 }, recipients = [] }, { height_range = { start = 0, end = 0 }, recipients = [] }, { height_range = { start = 0, end = 0 }, recipients = [] }, {}]"#,
            "height_range",
        ),
        (
            "missing default recipients",
            r#"funding_streams = [{ height_range = { start = 0, end = 0 }, recipients = [] }, { height_range = { start = 0, end = 0 }, recipients = [] }, { height_range = { start = 0, end = 0 }, recipients = [] }, { height_range = { start = 1, end = 2 } }]"#,
            "recipients",
        ),
        (
            "empty address extension",
            r#"extend_funding_stream_addresses_as_required = true
funding_streams = [{ height_range = { start = 1, end = 10 }, recipients = [{ receiver = "ECC", numerator = 7, addresses = [] }] }]"#,
            "addresses",
        ),
        (
            "empty addresses",
            r#"funding_streams = [{ height_range = { start = 1, end = 10 }, recipients = [{ receiver = "ECC", numerator = 7, addresses = [] }] }]"#,
            "addresses",
        ),
        (
            "insufficient addresses",
            r#"funding_streams = [{ height_range = { start = 1, end = 40000 }, recipients = [{ receiver = "ECC", numerator = 7, addresses = ["t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz"] }] }]"#,
            "addresses",
        ),
        (
            "wrong network address",
            r#"funding_streams = [{ height_range = { start = 0, end = 1 }, recipients = [{ receiver = "ECC", numerator = 7, addresses = ["t3LmX1cxWPPPqL4TZHx42HU3U5ghbFjRiif"] }] }]"#,
            "Testnet",
        ),
        (
            "malformed lockbox address",
            r#"lockbox_disbursements = [{ address = "invalid", amount = 1 }]"#,
            "lockbox disbursement address",
        ),
        (
            "excessive lockbox total",
            r#"lockbox_disbursements = [{ address = "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz", amount = 2100000000000000 }, { address = "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz", amount = 1 }]"#,
            "lockbox disbursement total",
        ),
    ];
    let mut failures = Vec::new();
    for network in ["Testnet", "Regtest"] {
        for (name, parameters, diagnostic) in cases {
            let isolation = if network == "Testnet" {
                "network_magic = [0, 0, 0, 0]\ncheckpoints = true\n"
            } else {
                ""
            };
            let config = format!(
                "network = '{network}'\ninitial_testnet_peers = []\n\
                 [testnet_parameters]\n{isolation}{parameters}"
            );
            match std::panic::catch_unwind(|| toml::from_str::<Config>(&config)) {
                Ok(Err(error)) if error.to_string().contains(diagnostic) => {}
                Ok(Err(error)) => failures.push(format!("{network}: {name}: {error}")),
                Ok(Ok(_)) => failures.push(format!("{network}: {name}: accepted invalid input")),
                Err(_) => failures.push(format!("{network}: {name}: panicked")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Validation preserves address extension, valid lockbox totals, and canonical serialization.
#[test]
fn valid_funding_and_lockbox_configuration_roundtrip() {
    let _init_guard = zebra_test::init();
    for network in ["Testnet", "Regtest"] {
        let isolation = if network == "Testnet" {
            "network_magic = [0, 0, 0, 0]\ncheckpoints = true\n"
        } else {
            ""
        };
        let config: Config = toml::from_str(&format!(
            "network = '{network}'\ninitial_testnet_peers = []\n\
             [testnet_parameters]\n{isolation}\
             extend_funding_stream_addresses_as_required = true\n\
             funding_streams = [\
               {{ height_range = {{ start = 0, end = 0 }}, recipients = [\
                 {{ receiver = 'ECC', numerator = 7 }}\
               ] }},\
               {{ height_range = {{ start = 1, end = 40000 }}, recipients = [\
                 {{ receiver = 'ECC', numerator = 7, addresses = ['t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz'] }},\
                 {{ receiver = 'Deferred', numerator = 93 }}\
               ] }}\
             ]\n\
             lockbox_disbursements = [\
               {{ address = 't26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz', amount = 2100000000000000 }}\
             ]"
        ))
        .unwrap();
        let restored: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, restored);
        let parameters = restored.network.parameters().unwrap();
        assert_eq!(
            parameters.lockbox_disbursements()[0].1,
            parameters.lockbox_disbursement_total_amount(),
        );
        for height in [1, 39_999] {
            assert_eq!(
                zebra_chain::parameters::subsidy::funding_stream_address(
                    Height(height),
                    &restored.network,
                    FundingStreamReceiver::Ecc,
                )
                .unwrap()
                .to_string(),
                "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
            );
        }
    }
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

#[test]
fn regtest_rejects_fixed_parameter_overrides() {
    let _init_guard = zebra_test::init();
    let canonical: Config = toml::from_str("network = 'Regtest'").unwrap();
    let restored: Config = toml::from_str(&toml::to_string(&canonical).unwrap()).unwrap();
    assert_eq!(canonical, restored);

    for (field, value) in [
        ("disable_pow", "false"),
        ("network_magic", "[0, 1, 2, 3]"),
        ("genesis_hash", "'invalid'"),
        (
            "genesis_hash",
            "'0000000000000000000000000000000000000000000000000000000000000000'",
        ),
        ("slow_start_interval", "1"),
        ("target_difficulty_limit", "'invalid'"),
        ("target_difficulty_limit", "'1'"),
        ("pre_blossom_halving_interval", "145"),
        ("temporary_orchard_disabling_soft_fork_height", "1"),
    ] {
        for prefix in [
            "network = 'Regtest'\n[testnet_parameters]",
            "[network.params]",
        ] {
            let config = format!("{prefix}\n{field} = {value}\n");
            let error = toml::from_str::<Config>(&config)
                .expect_err("Regtest must reject overrides of fixed parameters");
            assert!(
                error.to_string().contains(field),
                "unexpected error: {error}"
            );
        }
    }
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
        zebra_chain::parameters::subsidy::additional_block_subsidy(
            Height(11),
            &config.network,
            reserve
        )
        .zatoshis(),
        0,
    );
    assert!(
        zebra_chain::parameters::subsidy::additional_block_subsidy(
            Height(12),
            &config.network,
            reserve
        )
        .zatoshis()
            > 0
    );

    assert!(toml::from_str::<Config>(
        &configuration(12).replace("nsm_reissuance_height", "zip234_deployment_height")
    )
    .is_err());
    for height in [0, 8, u32::MAX] {
        assert!(toml::from_str::<Config>(&configuration(height)).is_err());
    }
    assert!(toml::from_str::<Config>(
        "network = 'Regtest'\n[testnet_parameters]\nnsm_reissuance_height = 12\n"
    )
    .is_err());
    let config: Config = toml::from_str(
        "network = 'Regtest'\n[testnet_parameters]\ninitial_nsm_value_balance = 100000000\n\
         [testnet_parameters.activation_heights]\nNU7 = 9\n",
    )
    .unwrap();
    assert_eq!(config.network.initial_nsm_value_balance(), reserve);
    let roundtrip: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(roundtrip.network, config.network);
    assert_eq!(
        zebra_chain::parameters::subsidy::additional_block_subsidy(
            Height(9),
            &roundtrip.network,
            reserve
        )
        .zatoshis(),
        0,
    );
}

#[test]
fn public_nsm_seed_survives_config_roundtrip_and_rejects_override() {
    for (configuration, is_public_testnet) in [
        ("network = 'Testnet'", true),
        (
            "network = 'Testnet'\n[testnet_parameters]\ncheckpoints = true\ninitial_nsm_value_balance = 55768414957\n",
            true,
        ),
        (
            "network = 'Testnet'\n[testnet_parameters]\nnetwork_name = 'MyTestnet'\ncheckpoints = true\n",
            false,
        ),
    ] {
        let public: Config = toml::from_str(configuration).unwrap();
        let roundtrip: Config = toml::from_str(&toml::to_string(&public).unwrap()).unwrap();
        assert_eq!(public.network, roundtrip.network);
        assert_eq!(roundtrip.network.is_default_testnet(), is_public_testnet);
        assert_eq!(
            roundtrip.network.initial_nsm_value_balance().zatoshis(),
            55_768_414_957
        );
    }
    let configuration = |seed| {
        format!("network = 'Testnet'\n[testnet_parameters]\ninitial_nsm_value_balance = {seed}\n")
    };
    assert!(toml::from_str::<Config>(&configuration(0)).is_err());

    let private: Config = toml::from_str(
        "network = 'Testnet'\ninitial_testnet_peers = []\n[testnet_parameters]\ncheckpoints = true\nnetwork_magic = [0, 0, 0, 0]\ninitial_nsm_value_balance = 55768414957\n",
    )
    .unwrap();
    assert_eq!(
        private.network.initial_nsm_value_balance().zatoshis(),
        55_768_414_957,
    );
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
    assert_eq!(private.network.initial_nsm_value_balance().zatoshis(), 0);
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
fn legacy_public_testnet_overrides_require_migration() {
    let public: Config = toml::from_str(
        "network = 'Testnet'\ninitial_testnet_peers = []\n\
         [testnet_parameters]\ncheckpoints = true\n",
    )
    .unwrap();
    assert!(public.network.is_default_testnet());

    let mut without_nu7 = public.clone();
    let mut activation_heights: testnet::ConfiguredActivationHeights =
        public.network.activation_list().into();
    activation_heights.nu7 = None;
    without_nu7.network = testnet::Parameters::build()
        .with_activation_heights(activation_heights)
        .unwrap()
        .to_network()
        .unwrap();

    let mut legacy_funding = public.clone();
    let mut streams: Vec<ConfiguredFundingStreams> = public
        .network
        .all_funding_streams()
        .iter()
        .map(Into::into)
        .collect();
    streams
        .last_mut()
        .unwrap()
        .height_range
        .as_mut()
        .unwrap()
        .end = Height(4_476_000);
    legacy_funding.network = testnet::Parameters::build()
        .with_funding_streams(streams)
        .to_network()
        .unwrap();

    for legacy in [without_nu7, legacy_funding] {
        let mut serialized: toml::Value =
            toml::from_str(&toml::to_string(&legacy).unwrap()).unwrap();
        assert!(serialized.clone().try_into::<Config>().is_err());
        serialized["network"].as_table_mut().unwrap().insert(
            "network_magic".to_string(),
            toml::Value::Array([0, 0, 0, 0].into_iter().map(toml::Value::Integer).collect()),
        );
        let isolated: Config = serialized.try_into().unwrap();
        assert_eq!(isolated.network.magic(), Magic([0; 4]));
        assert!(!isolated.network.is_default_testnet());
    }
}

#[test]
fn empty_funding_ranges_allow_address_auto_extension() {
    for (network, network_magic) in [("Testnet", [0; 4]), ("Regtest", magics::REGTEST.0)] {
        let config: Config = toml::from_str(&format!(
            "network = '{network}'\n\
             initial_testnet_peers = []\n\
             [testnet_parameters]\n\
             checkpoints = {}\n\
             network_magic = {network_magic:?}\n\
             extend_funding_stream_addresses_as_required = true\n\
             funding_streams = [\
               {{ height_range = {{ start = 0, end = 0 }}, recipients = [{{ receiver = 'ECC', numerator = 7, addresses = [] }}] }},\
               {{ height_range = {{ start = 1, end = 2 }}, recipients = [{{ receiver = 'Deferred', numerator = 12 }}] }}\
             ]\n",
            network == "Testnet",
        ))
        .unwrap();
        assert!(config.network.funding_streams(Height(0)).is_none());
        assert_eq!(
            config
                .network
                .funding_streams(Height(1))
                .unwrap()
                .recipients()[&FundingStreamReceiver::Deferred]
                .numerator(),
            12,
        );
        assert!(
            config.network.all_funding_streams()[0].recipients()[&FundingStreamReceiver::Ecc]
                .addresses()
                .is_empty()
        );
    }
}

#[test]
fn empty_funding_streams_reject_legacy_declarations() {
    let _init_guard = zebra_test::init();

    for (network, network_magic) in [("Testnet", [0; 4]), ("Regtest", magics::REGTEST.0)] {
        let checkpoints = network == "Testnet";
        for legacy_field in ["pre_nu6_funding_streams", "post_nu6_funding_streams"] {
            let config = format!(
                "network = '{network}'\n\
                 initial_testnet_peers = []\n\
                 [testnet_parameters]\n\
                 checkpoints = {checkpoints}\n\
                 network_magic = {network_magic:?}\n\
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
fn legacy_funding_streams_combine_with_disjoint_nonempty_lists() {
    let _init_guard = zebra_test::init();

    for (network, network_magic) in [("Testnet", [0; 4]), ("Regtest", magics::REGTEST.0)] {
        let checkpoints = network == "Testnet";
        let config: Config = toml::from_str(&format!(
            "network = '{network}'\n\
             initial_testnet_peers = []\n\
             [testnet_parameters]\n\
             checkpoints = {checkpoints}\n\
             network_magic = {network_magic:?}\n\
             pre_nu6_funding_streams = {{ height_range = {{ start = 1, end = 4 }}, recipients = [{{ receiver = 'Deferred', numerator = 1 }}] }}\n\
             post_nu6_funding_streams = {{ height_range = {{ start = 4, end = 5 }}, recipients = [{{ receiver = 'Deferred', numerator = 2 }}] }}\n\
             funding_streams = [{{ height_range = {{ start = 5, end = 6 }}, recipients = [{{ receiver = 'Deferred', numerator = 3 }}] }}]\n"
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

#[test]
fn legacy_post_nu6_defaults_do_not_depend_on_pre_nu6_streams() {
    let defaults = testnet::Parameters::default();
    let expected = &defaults.funding_streams()[1];

    for pre_nu6 in ["", "pre_nu6_funding_streams = {}\n"] {
        let config: Config = toml::from_str(&format!(
            "network = 'Testnet'\n\
             initial_testnet_peers = []\n\
             [testnet_parameters]\n\
             network_magic = [0, 0, 0, 0]\n\
             checkpoints = true\n\
             {pre_nu6}\
             post_nu6_funding_streams = {{}}\n"
        ))
        .unwrap();
        assert_eq!(
            config
                .network
                .funding_streams(expected.height_range().start),
            Some(expected),
        );
    }
}

#[test]
fn incompatible_testnet_rejects_public_seed_aliases() {
    for seed in [
        "DNSSEED.TESTNET.Z.CASH:18233",
        "dnsseed.testnet.z.cash.:18233",
        "DNSSEED.Z.CASH.:8233",
    ] {
        let config = format!(
            "network = 'Testnet'\n\
             initial_testnet_peers = ['{seed}']\n\
             [testnet_parameters]\n\
             network_magic = [0, 0, 0, 0]\n\
             checkpoints = true\n\
             disable_pow = true\n"
        );
        assert!(toml::from_str::<Config>(&config).is_err(), "{seed}");
    }
}

#[test]
fn configured_testnets_have_isolated_peer_caches() {
    let cache = crate::config::CacheDir::custom_path("cache");
    let networks = [0, 1].map(|byte| {
        testnet::Parameters::build()
            .with_network_magic(Magic([byte; 4]))
            .unwrap()
            .to_network()
            .unwrap()
    });
    assert_ne!(
        cache.peer_cache_file_path(&networks[0]),
        cache.peer_cache_file_path(&networks[1]),
    );
    for network in networks {
        assert_ne!(
            cache.peer_cache_file_path(&network),
            cache.peer_cache_file_path(&Network::new_default_testnet()),
        );
    }
}

#[tokio::test]
async fn regtest_skips_public_seed_resolution() {
    let mut config = Config {
        network: Network::new_regtest(Default::default()),
        ..Config::default()
    };
    assert!(config.initial_peer_hostnames().is_empty());
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), config.initial_peers())
            .await
            .unwrap()
            .is_empty()
    );

    config
        .initial_testnet_peers
        .insert("127.0.0.1:18233".into());
    let peers = tokio::time::timeout(std::time::Duration::from_secs(1), config.initial_peers())
        .await
        .unwrap();
    assert_eq!(peers, ["127.0.0.1:18233".parse().unwrap()].into());
}

/// Test that failing DNS doesn't stop Zebra loading its cached peers.
#[tokio::test(start_paused = true)]
async fn failing_dns_returns_cached_peers() {
    let _init_guard = zebra_test::init();

    let peer_cache_dir =
        tempfile::tempdir().expect("creating a temporary cache directory should succeed");
    let config = Config {
        cache_dir: crate::config::CacheDir::custom_path(peer_cache_dir.path()),
        // The `.invalid` top-level domain never resolves.
        initial_mainnet_peers: ["dns-failure.invalid:8233".to_string()].into(),
        ..Config::default()
    };
    let cached_peers: std::collections::HashSet<crate::PeerSocketAddr> =
        ["127.1.1.1:8233", "127.1.1.2:8233"]
            .into_iter()
            .map(|addr| addr.parse().expect("test peer addresses are valid"))
            .collect();
    config
        .update_peer_cache(cached_peers.clone())
        .await
        .expect("writing the temporary peer cache should succeed");

    let peers = tokio::time::timeout(
        crate::constants::DNS_LOOKUP_TIMEOUT * 3,
        config.initial_peers(),
    )
    .await
    .expect("initial peers are returned without retrying DNS when there are cached peers");

    assert_eq!(peers, cached_peers);
}

/// The path-taking peer cache functions read and write the same files as the ones that use the
/// configured cache path, and leave other files alone.
#[tokio::test]
async fn peer_cache_at_a_path_matches_the_configured_cache() {
    let _init_guard = zebra_test::init();

    let peer_cache_dir =
        tempfile::tempdir().expect("creating a temporary cache directory should succeed");
    let config = Config {
        cache_dir: crate::config::CacheDir::custom_path(peer_cache_dir.path()),
        ..Config::default()
    };
    let configured_path = config
        .cache_dir
        .peer_cache_file_path(&config.network)
        .expect("the cache directory is enabled");
    let other_path = peer_cache_dir.path().join("other.peers");

    let peers = |addrs: &[&str]| -> std::collections::HashSet<crate::PeerSocketAddr> {
        addrs
            .iter()
            .map(|addr| addr.parse().expect("test peer addresses are valid"))
            .collect()
    };
    let configured_peers = peers(&["127.1.1.1:8233", "127.1.1.2:8233"]);
    let other_peers = peers(&["127.2.2.2:8233"]);

    config
        .update_peer_cache(configured_peers.clone())
        .await
        .expect("writing the temporary peer cache should succeed");
    config
        .update_peer_cache_at(other_path.clone(), other_peers.clone())
        .await
        .expect("writing the temporary peer cache should succeed");

    assert_eq!(
        config
            .load_peer_cache_at(&configured_path)
            .await
            .expect("the configured cache exists"),
        configured_peers,
    );
    assert_eq!(
        config.load_peer_cache().await.expect("the cache exists"),
        configured_peers,
        "writing another path leaves the configured cache alone",
    );
    assert_eq!(
        config
            .load_peer_cache_at(&other_path)
            .await
            .expect("the other cache exists"),
        other_peers,
    );

    // An empty list keeps the previous file, and a missing file loads as empty.
    config
        .update_peer_cache_at(other_path.clone(), Default::default())
        .await
        .expect("an empty list is not written");
    assert_eq!(
        config
            .load_peer_cache_at(&other_path)
            .await
            .expect("the other cache exists"),
        other_peers,
    );
    assert!(config
        .load_peer_cache_at(&peer_cache_dir.path().join("missing.peers"))
        .await
        .expect("a missing cache is not an error")
        .is_empty());
}

/// The advertised version 2 address is the external IP with the version 2 port, or a specific
/// version 2 listener, and never an unspecified listener.
#[test]
fn v2_advertised_addr_prefers_the_external_ip() {
    let _init_guard = zebra_test::init();

    let config = |v2_listen_addr: Option<&str>, external_addr: Option<&str>| Config {
        v2_listen_addr: v2_listen_addr.map(|addr| addr.parse().expect("valid address")),
        external_addr: external_addr.map(|addr| addr.parse().expect("valid address")),
        ..Config::default()
    };
    let addr = |addr: &str| -> std::net::SocketAddr { addr.parse().expect("valid address") };

    assert_eq!(
        config(None, Some("203.0.113.1:8233")).v2_advertised_addr(),
        None
    );
    assert_eq!(
        config(Some("[::]:8233"), Some("203.0.113.1:9999")).v2_advertised_addr(),
        Some(addr("203.0.113.1:8233")),
    );
    assert_eq!(
        config(Some("198.51.100.1:8233"), None).v2_advertised_addr(),
        Some(addr("198.51.100.1:8233")),
    );
    assert_eq!(config(Some("[::]:8233"), None).v2_advertised_addr(), None);
    assert_eq!(
        config(Some("0.0.0.0:8233"), None).v2_advertised_addr(),
        None
    );
}

/// Version 2 discovery is on by default, survives a round trip, and dials a share of the
/// initial target size.
#[test]
fn v2_peer_discovery_defaults_on_and_round_trips() {
    let _init_guard = zebra_test::init();

    let default = Config::default();
    assert!(default.v2_peer_discovery);

    let config: Config = toml::from_str("v2_peer_discovery = false").expect("a valid config");
    assert!(!config.v2_peer_discovery);
    let round_trip: Config =
        toml::from_str(&toml::to_string(&config).expect("serializes")).expect("deserializes");
    assert_eq!(round_trip, config);

    let v2 = Config {
        v2_listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        ..Config::default()
    };
    assert_eq!(v2.v2_outbound_target(), v2.peerset_initial_target_size / 2);
    let tiny = Config {
        peerset_initial_target_size: 1,
        ..v2
    };
    assert_eq!(tiny.v2_outbound_target(), 1);
    assert_eq!(
        Config::default().v2_outbound_target(),
        0,
        "no endpoint, no slots"
    );
}
