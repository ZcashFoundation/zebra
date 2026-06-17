//! Fixed test vectors for zebra-network configuration.

use std::time::Duration;

use static_assertions::const_assert;
use zebra_chain::{
    block::Height,
    parameters::{
        testnet::{self, ConfiguredFundingStreams},
        Network,
    },
};

use crate::{
    constants::{INBOUND_PEER_LIMIT_MULTIPLIER, OUTBOUND_PEER_LIMIT_MULTIPLIER},
    zakura::{DEFAULT_HS_MAX_INFLIGHT, DEFAULT_HS_RANGE},
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

    // This fork prioritizes fast outbound sync over inbound-serving capacity.
    const_assert!(INBOUND_PEER_LIMIT_MULTIPLIER <= OUTBOUND_PEER_LIMIT_MULTIPLIER);

    let config = Config::default();

    assert!(
        config.peerset_inbound_connection_limit() <= config.peerset_outbound_connection_limit(),
        "this fork caps inbound connections at or below the outbound limit, to prioritize sync",
    );
}

#[test]
fn testnet_params_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: testnet::Parameters::build()
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
fn zakura_node_secret_key_is_redacted_from_debug_and_serialization() {
    let _init_guard = zebra_test::init();

    let secret = "not-a-real-iroh-secret-but-sensitive";
    let config: Config = toml::from_str(&format!("zakura_node_secret_key = '{secret}'")).unwrap();

    assert_eq!(
        config
            .zakura_node_secret_key
            .as_ref()
            .expect("test config should parse the Zakura secret key")
            .expose_secret(),
        secret
    );

    let debug = format!("{config:?}");
    assert!(debug.contains("zakura_node_secret_key"));
    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains(secret));

    let serialized = toml::to_string(&config).unwrap();
    assert!(!serialized.contains("zakura_node_secret_key"));
    assert!(!serialized.contains(secret));
}

#[test]
fn p2p_protocol_flags_default_on_and_roundtrip() {
    let _init_guard = zebra_test::init();

    assert!(Config::default().v2_p2p);
    assert!(Config::default().legacy_p2p);
    assert!(Config::default().zakura.bootstrap_peers.is_empty());

    let config: Config = toml::from_str(
        r#"
        v2_p2p = false
        legacy_p2p = false
        "#,
    )
    .unwrap();
    assert!(!config.v2_p2p);
    assert!(!config.legacy_p2p);

    let serialized = toml::to_string(&config).unwrap();
    assert!(serialized.contains("v2_p2p = false"));
    assert!(serialized.contains("legacy_p2p = false"));

    let deserialized: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(config, deserialized);
}

#[test]
fn p2p_v2_old_enable_config_alias_still_parses() {
    let _init_guard = zebra_test::init();

    let config: Config = toml::from_str("enable_p2p_v2 = false").unwrap();

    assert!(!config.v2_p2p);
    assert!(config.legacy_p2p);
}

#[test]
fn p2p_v2_old_config_without_zakura_fields_uses_safe_defaults() {
    let _init_guard = zebra_test::init();

    let config: Config = toml::from_str(
        r#"
        listen_addr = "127.0.0.1:8233"
        peerset_initial_target_size = 25
        "#,
    )
    .unwrap();

    assert_eq!(config.listen_addr.to_string(), "127.0.0.1:8233");
    assert!(config.v2_p2p);
    assert!(config.legacy_p2p);
    assert!(config.zakura.bootstrap_peers.is_empty());
    assert!(config.zakura.max_connections > 0);
    assert!(config.zakura.max_pending_handshakes > 0);
    assert_eq!(
        config.zakura.header_sync.max_headers_per_response,
        DEFAULT_HS_RANGE
    );
    assert_eq!(
        config.zakura.header_sync.max_inflight_requests,
        DEFAULT_HS_MAX_INFLIGHT
    );
    assert_eq!(
        config.zakura.header_sync.status_refresh_interval,
        Duration::from_secs(30)
    );
    assert_eq!(config.zakura.header_sync.anchor_height, None);
    assert_eq!(config.zakura.header_sync.anchor_hash, None);
}

#[test]
fn p2p_v2_unknown_future_config_fields_are_rejected() {
    let _init_guard = zebra_test::init();

    let top_level = toml::from_str::<Config>("future_zakura_field = true")
        .expect_err("deny_unknown_fields rejects unknown top-level fields");
    assert!(
        top_level.to_string().contains("unknown field"),
        "unexpected error for unknown top-level field: {top_level}",
    );

    let nested = toml::from_str::<Config>(
        r#"
        [zakura]
        future_field = true
        "#,
    )
    .expect_err("deny_unknown_fields rejects unknown Zakura fields");
    assert!(
        nested.to_string().contains("unknown field"),
        "unexpected error for unknown nested field: {nested}",
    );

    let header_sync = toml::from_str::<Config>(
        r#"
        [zakura.header_sync]
        future_field = true
        "#,
    )
    .expect_err("deny_unknown_fields rejects unknown header-sync fields");
    assert!(
        header_sync.to_string().contains("unknown field"),
        "unexpected error for unknown header-sync field: {header_sync}",
    );

    let block_sync = toml::from_str::<Config>(
        r#"
        [zakura.block_sync]
        future_field = true
        "#,
    )
    .expect_err("deny_unknown_fields rejects unknown block-sync fields");
    assert!(
        block_sync.to_string().contains("unknown field"),
        "unexpected error for unknown block-sync field: {block_sync}",
    );
}

#[test]
fn p2p_v2_config_roundtrip_keeps_dconfig_zakura_fields() {
    let _init_guard = zebra_test::init();

    let config: Config = toml::from_str(
        r#"
        v2_p2p = true
        legacy_p2p = true

        [zakura]
        bootstrap_peers = ["ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6@127.0.0.1:8233"]
        max_connections = 7
        max_pending_handshakes = 3
        stream_open_rate_per_second = 11
        message_rate_per_second = 13
        trace_dir = "target/zakura-test-traces"

        [zakura.header_sync]
        max_headers_per_response = 333
        max_inflight_requests = 9
        status_refresh_interval = "45s"

        [zakura.block_sync]
        replace_legacy_syncer = true
        max_blocks_per_response = 5
        status_refresh_interval = "12s"
        "#,
    )
    .unwrap();

    let serialized = toml::to_string(&config).unwrap();
    assert!(serialized.contains("v2_p2p = true"));
    assert!(serialized.contains("legacy_p2p = true"));
    assert!(serialized.contains("[zakura]"));
    assert!(serialized.contains("bootstrap_peers"));
    assert!(serialized.contains("max_connections = 7"));
    assert!(serialized.contains("trace_dir = \"target/zakura-test-traces\""));
    assert!(serialized.contains("[zakura.header_sync]"));
    assert!(serialized.contains("max_headers_per_response = 333"));
    assert!(serialized.contains("max_inflight_requests = 9"));
    assert!(serialized.contains("status_refresh_interval = \"45s\""));
    assert!(serialized.contains("[zakura.block_sync]"));
    assert!(serialized.contains("replace_legacy_syncer = true"));
    assert!(serialized.contains("max_blocks_per_response = 5"));
    assert!(serialized.contains("status_refresh_interval = \"12s\""));
    assert_eq!(toml::from_str::<Config>(&serialized).unwrap(), config);
}

#[test]
fn zakura_bootstrap_peers_parse_in_nested_config() {
    let _init_guard = zebra_test::init();

    let config: Config = toml::from_str(
        r#"
        v2_p2p = true

        [zakura]
        bootstrap_peers = ["ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6@127.0.0.1:8233"]
        max_connections = 4
        max_pending_handshakes = 2
        stream_open_rate_per_second = 3
        message_rate_per_second = 5
        "#,
    )
    .unwrap();

    assert!(config.v2_p2p);
    assert!(config.legacy_p2p);
    assert_eq!(config.zakura.bootstrap_peers.len(), 1);
    assert_eq!(config.zakura.max_connections, 4);
    assert_eq!(config.zakura.max_pending_handshakes, 2);
    assert_eq!(config.zakura.stream_open_rate_per_second, 3);
    assert_eq!(config.zakura.message_rate_per_second, 5);
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

/// Checks that a configured Testnet's temporary Orchard-disabling soft fork height
/// survives a serialization round-trip.
#[test]
fn temporary_orchard_disabling_soft_fork_height_serialization_roundtrip() {
    let _init_guard = zebra_test::init();

    let soft_fork_height = Height(2_000_000);

    let config = Config {
        network: testnet::Parameters::build()
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
