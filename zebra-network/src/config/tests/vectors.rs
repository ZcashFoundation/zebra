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
    zakura::{DEFAULT_HS_MAX_INFLIGHT, DEFAULT_HS_RANGE, DEFAULT_ZAKURA_LISTEN_ADDR},
    CacheDir, Config,
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
    assert!(!serialized.contains("replace_legacy_syncer"));
    assert!(serialized.contains("max_blocks_per_response = 5"));
    assert!(serialized.contains("status_refresh_interval = \"12s\""));
    assert_eq!(toml::from_str::<Config>(&serialized).unwrap(), config);
    assert!(
        !config.zakura.block_sync.replace_legacy_syncer,
        "deprecated replace_legacy_syncer config is accepted but ignored"
    );
}

#[test]
fn configured_regtest_checkpoints_preserve_regtest_identity() {
    let _init_guard = zebra_test::init();

    // Mirrors the per-node config the zakura-regtest-e2e harness writes for the from-scratch
    // catch-up node: a Regtest node that overrides only the checkpoint list (derived at
    // runtime from the miner's chain). Regtest identity — genesis hash and network magic —
    // must be preserved so the node still peers with a plain-Regtest miner; only checkpoint
    // verification is added.
    let genesis = Network::new_regtest(Default::default()).genesis_hash();
    let checkpoint = zebra_chain::block::Hash([7; 32]);

    // The exact minimal `[network.params]` table the harness writes: it overrides only the
    // checkpoint list and lets every other Regtest parameter default. `block::Hash`
    // serializes as a 32-byte array in internal (display-reversed) order, so the harness must
    // emit byte arrays, not hex — this asserts that exact form parses.
    let bytes_csv = |hash: zebra_chain::block::Hash| {
        hash.0
            .iter()
            .map(|byte| byte.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    // The harness rewrites node2's `network = "Regtest"` line in place with this inline table
    // (a single-line `sed` replacement), so verify exactly that form.
    let inline = format!(
        "network = {{ params = {{ checkpoints = [[0, [{}]], [10, [{}]]] }} }}\n",
        bytes_csv(genesis),
        bytes_csv(checkpoint),
    );

    let config: Config = toml::from_str(&inline)
        .expect("the harness's inline ConfiguredRegtest checkpoint TOML deserializes");

    assert!(
        config.network.is_regtest(),
        "a checkpoint-only override must stay Regtest",
    );
    assert_eq!(
        config.network.genesis_hash(),
        genesis,
        "Regtest genesis hash is preserved, so the node still peers with a plain-Regtest miner",
    );

    let checkpoints = config.network.checkpoint_list();
    assert_eq!(
        checkpoints.max_height(),
        Height(10),
        "the derived checkpoint list replaces the genesis-only Regtest default",
    );
    assert_eq!(checkpoints.hash(Height(0)), Some(genesis));
    assert_eq!(checkpoints.hash(Height(10)), Some(checkpoint));
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
    assert_eq!(config.zakura.listen_addr, Some(DEFAULT_ZAKURA_LISTEN_ADDR));
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

/// With `v2_p2p` enabled, default config (no `zakura_node_secret_key`), and a
/// writable cache dir, the generated Zakura iroh identity must be persisted on
/// first use and reused on every later startup, so the node's `NodeId` is stable
/// across restarts.
///
/// This is the regression test for `claude-ephemeral-node-secret-on-restart`:
/// before the fix, `Config::zakura_secret_key` generated a fresh ephemeral key on
/// every call and never wrote the reserved cache-dir key file, so two startups
/// produced different `NodeId`s and no key file existed.
#[test]
fn zakura_secret_key_is_persisted_and_stable_across_restarts() {
    let _init_guard = zebra_test::init();

    let cache_dir = tempfile::tempdir().expect("failed to create temp cache dir");

    let config = Config {
        cache_dir: CacheDir::custom_path(cache_dir.path()),
        zakura_node_secret_key: None,
        v2_p2p: true,
        ..Config::default()
    };

    let key_file = config
        .cache_dir
        .zakura_node_secret_key_file_path(&config.network)
        .expect("an enabled cache dir must yield a key file path");

    // The key file must not exist before first use.
    assert!(
        !key_file.exists(),
        "key file should not exist before the first startup",
    );

    // First startup: generate and persist a fresh key.
    let first = config
        .zakura_secret_key()
        .expect("default config should resolve a secret key");

    // The reserved cache-dir key file must now exist (atomic create+persist).
    assert!(
        key_file.exists(),
        "first startup must persist the generated key to the cache-dir key file",
    );

    // On Unix, the long-term private identity file must be owner-only (0o600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file)
            .expect("key file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "persisted secret key file must be owner-only");
    }

    // Second startup with a fresh `Config` reading the same cache dir (simulating
    // a process restart) must reuse the persisted key, yielding the same `NodeId`.
    let restart_config = Config {
        cache_dir: CacheDir::custom_path(cache_dir.path()),
        zakura_node_secret_key: None,
        v2_p2p: true,
        ..Config::default()
    };
    let after_restart = restart_config
        .zakura_secret_key()
        .expect("restart should resolve the persisted secret key");

    assert_eq!(
        first.public(),
        after_restart.public(),
        "node identity must be stable across restarts when persisted to the cache dir",
    );

    // Calling again on the same config must also be stable.
    let again = config
        .zakura_secret_key()
        .expect("repeat resolution should succeed");
    assert_eq!(
        first.public(),
        again.public(),
        "repeat resolution must reuse the persisted key",
    );
}

/// A configured `zakura_node_secret_key` must always win and is never overwritten
/// by the cache-dir persistence path; a disabled cache dir falls back to an
/// ephemeral key without writing any file.
#[test]
fn zakura_secret_key_honors_configured_key_and_disabled_cache() {
    let _init_guard = zebra_test::init();

    let cache_dir = tempfile::tempdir().expect("failed to create temp cache dir");

    // Persist a key first so a key file exists in the cache dir.
    let persisting = Config {
        cache_dir: CacheDir::custom_path(cache_dir.path()),
        zakura_node_secret_key: None,
        v2_p2p: true,
        ..Config::default()
    };
    let persisted = persisting.zakura_secret_key().expect("persist a key");

    // A configured key (64-char lowercase hex of the all-ones secret) must override
    // the persisted cache-dir key.
    let configured = "01".repeat(32);
    let mut with_key: Config = toml::from_str(&format!("zakura_node_secret_key = '{configured}'"))
        .expect("valid configured key parses");
    with_key.cache_dir = CacheDir::custom_path(cache_dir.path());
    let from_config = with_key
        .zakura_secret_key()
        .expect("configured key should resolve");
    assert_ne!(
        from_config.public(),
        persisted.public(),
        "configured key must override the persisted cache-dir key",
    );

    // A disabled cache dir cannot persist, so it yields an ephemeral key and writes
    // no file.
    let disabled = Config {
        cache_dir: CacheDir::disabled(),
        zakura_node_secret_key: None,
        v2_p2p: true,
        ..Config::default()
    };
    assert!(
        disabled
            .cache_dir
            .zakura_node_secret_key_file_path(&disabled.network)
            .is_none(),
        "disabled cache dir must not yield a key file path",
    );
    // Resolving still succeeds (ephemeral), and successive calls may differ.
    disabled
        .zakura_secret_key()
        .expect("disabled cache dir should still resolve an ephemeral key");
}
