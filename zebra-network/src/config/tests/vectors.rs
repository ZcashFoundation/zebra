//! Fixed test vectors for zebra-network configuration.

use static_assertions::const_assert;
use zebra_chain::{
    block::Height,
    parameters::{
        subsidy::{
            FundingStreamReceiver, FundingStreamRecipient, FundingStreams, FUNDING_STREAMS_TESTNET,
            FUNDING_STREAM_ECC_ADDRESSES_TESTNET, FUNDING_STREAM_MG_ADDRESSES_TESTNET,
            FUNDING_STREAM_ZF_ADDRESSES_TESTNET, POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_TESTNET,
        },
        testnet::{self, ConfiguredFundingStreamRecipient, ConfiguredFundingStreams},
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

    let fs = vec![
        ConfiguredFundingStreams::from(&FundingStreams::new(
            Height(1_028_500)..Height(2_796_000),
            vec![
                (
                    FundingStreamReceiver::Ecc,
                    FundingStreamRecipient::new(7, FUNDING_STREAM_ECC_ADDRESSES_TESTNET),
                ),
                (
                    FundingStreamReceiver::ZcashFoundation,
                    FundingStreamRecipient::new(5, FUNDING_STREAM_ZF_ADDRESSES_TESTNET),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, FUNDING_STREAM_MG_ADDRESSES_TESTNET),
                ),
            ]
            .into_iter()
            .collect(),
        )),
        ConfiguredFundingStreams::from(&FundingStreams::new(
            Height(2_976_000)..Height(2_796_000 + 420_000),
            vec![
                (
                    FundingStreamReceiver::Deferred,
                    FundingStreamRecipient::new::<[&str; 0], &str>(12, []),
                ),
                (
                    FundingStreamReceiver::MajorGrants,
                    FundingStreamRecipient::new(8, POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_TESTNET),
                ),
            ]
            .into_iter()
            .collect(),
        )),
    ];

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
fn funding_streams_default_values() {
    let _init_guard = zebra_test::init();

    let fs = vec![
        ConfiguredFundingStreams {
            height_range: Some(Height(1_028_500 - 1)..Height(2_796_000 - 1)),
            // Will read from existing values
            recipients: None,
        },
        ConfiguredFundingStreams {
            // Will read from existing values
            height_range: None,
            recipients: Some(vec![
                ConfiguredFundingStreamRecipient {
                    receiver: FundingStreamReceiver::Deferred,
                    numerator: 1,
                    addresses: None,
                },
                ConfiguredFundingStreamRecipient {
                    receiver: FundingStreamReceiver::MajorGrants,
                    numerator: 2,
                    addresses: Some(
                        POST_NU6_FUNDING_STREAM_FPF_ADDRESSES_TESTNET
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    ),
                },
            ]),
        },
    ];

    let network = testnet::Parameters::build()
        .with_funding_streams(fs)
        .to_network()
        .expect("failed to build configured network");

    // Check if value hasn't changed
    assert_eq!(
        network.all_funding_streams()[0].height_range().clone(),
        Height(1_028_500 - 1)..Height(2_796_000 - 1)
    );
    // Check if value was copied from default
    assert_eq!(
        network.all_funding_streams()[0]
            .recipients()
            .get(&FundingStreamReceiver::ZcashFoundation)
            .unwrap()
            .addresses(),
        FUNDING_STREAMS_TESTNET[0]
            .recipients()
            .get(&FundingStreamReceiver::ZcashFoundation)
            .unwrap()
            .addresses()
    );
    // Check if value was copied from default
    assert_eq!(
        network.all_funding_streams()[1].height_range(),
        FUNDING_STREAMS_TESTNET[1].height_range()
    );
    // Check if value hasn't changed
    assert_eq!(
        network.all_funding_streams()[1]
            .recipients()
            .get(&FundingStreamReceiver::Deferred)
            .unwrap()
            .numerator(),
        1
    );
}
